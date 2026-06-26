use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use miku::markdown::{extract_title, parse_frontmatter, render_html};
use minijinja::{context, Environment};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{info, warn};

#[derive(serde::Serialize)]
struct Backlink {
    path: String,
    title: String,
}

#[derive(serde::Serialize)]
struct TagCount {
    tag: String,
    count: i64,
}

#[derive(serde::Serialize)]
struct PageRef {
    path: String,
    title: String,
}

#[derive(serde::Serialize)]
struct NavNode {
    name: String,         // folder segment name, or page title for leaves
    path: Option<String>, // Some(slug-path without .md) for pages; None for folders
    children: Vec<NavNode>,
}

#[derive(Clone)]
struct AppState {
    db: sqlx::PgPool,
    templates: Arc<Environment<'static>>,
}

// Custom error handling for Axum route handlers
struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        warn!("Handler error: {:?}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Initialize tracing with an env filter
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("info,miku=debug,tower_http=debug")
            }),
        )
        .init();

    info!("Starting Miku Server...");

    // 2. Load DATABASE_URL from environment variable
    let database_url =
        env::var("DATABASE_URL").context("DATABASE_URL environment variable must be set")?;

    // 3. Connect to PostgreSQL database using sqlx PgPool
    info!("Connecting to database...");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("Failed to connect to database")?;

    // 4. Run migrations on startup
    info!("Running database migrations...");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("Database migrations failed")?;
    info!("Migrations complete.");

    // 5. Initialize Minijinja template environment
    let mut templates_env = Environment::new();
    templates_env.set_loader(minijinja::path_loader("src/templates"));
    let state = AppState {
        db: pool.clone(),
        templates: Arc::new(templates_env),
    };

    // 6. Initialize background indexer
    let _indexer = miku::indexer::IndexerQueue::new(pool, std::path::PathBuf::from("miku"))
        .context("Failed to initialize background indexer")?;

    // 7. Build Router & Configure axum routes
    let app = app(state);

    // 8. Bind and run local listener to 0.0.0.0
    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    info!("Listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(redirect_to_index))
        .route("/search", get(search))
        .route("/tags", get(tags_index))
        .route("/tags/{tag}", get(tag_filter))
        .route("/p/{*path}", get(page_handler).post(page_save))
        .route("/api/move", post(page_move))
        .route("/api/trash", post(page_trash))
        .nest_service("/static", ServeDir::new("static"))
        .nest_service("/assets", ServeDir::new("miku/assets"))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// Redirect root "/" to "/p/Index"
async fn redirect_to_index() -> impl IntoResponse {
    Redirect::temporary("/p/Index")
}

// Helper to get safe path under miku/ and check for directory traversal
fn safe_file_path(path: &str) -> Result<PathBuf, AppError> {
    if path.contains("..") || path.starts_with('/') {
        return Err(anyhow::anyhow!("Invalid path: path traversal detected").into());
    }
    Ok(StdPath::new("miku").join(format!("{path}.md")))
}

// Helper to compute SHA-256 hash of content
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// Helper struct for building nav tree (internal use only)
#[derive(Debug)]
struct TreeNode {
    title: String,
    children: std::collections::BTreeMap<String, TreeNode>,
    is_leaf: bool,
}

// Convert TreeNode BTreeMap tree into Vec<NavNode> with sorting.
// Folders come first (sorted alphabetically), then pages (sorted alphabetically).
fn tree_to_nav_nodes(
    tree: std::collections::BTreeMap<String, TreeNode>,
    prefix: String,
) -> Vec<NavNode> {
    let mut folders = Vec::new();
    let mut pages = Vec::new();

    for (name, node) in tree {
        let current_path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };

        let children = tree_to_nav_nodes(node.children, current_path.clone());

        if node.is_leaf {
            pages.push(NavNode {
                name: node.title.clone(),
                path: Some(current_path.clone()),
                children,
            });
        } else {
            folders.push(NavNode {
                name: node.title.clone(),
                path: None,
                children,
            });
        }
    }

    // Sort: folders first (by name, case-insensitive), then pages (by name, case-insensitive)
    folders.sort_by_key(|a| a.name.to_lowercase());
    pages.sort_by_key(|a| a.name.to_lowercase());

    let mut result = folders;
    result.extend(pages);
    result
}

// Build a nested tree structure from page rows (path_without_md, title).
// Pure function, no DB, no async. Folders come first (sorted alphabetically),
// then pages (sorted alphabetically by name). Each row's path is like "a" or
// "b/c" or "b/d/e" (no .md). The final segment is a page leaf with path =
// Some(full path) and name = title; intermediate segments are folders with
// path = None.
fn build_nav_tree(rows: Vec<(String, String)>) -> Vec<NavNode> {
    use std::collections::BTreeMap;

    let mut root: BTreeMap<String, TreeNode> = BTreeMap::new();

    for (path, title) in rows {
        let parts: Vec<&str> = path.split('/').collect();

        // Navigate/create the tree structure
        let mut current = &mut root;
        for (i, &part) in parts.iter().enumerate() {
            let is_final = i == parts.len() - 1;

            if !current.contains_key(part) {
                current.insert(
                    part.to_string(),
                    TreeNode {
                        title: if is_final {
                            title.clone()
                        } else {
                            part.to_string()
                        },
                        children: BTreeMap::new(),
                        is_leaf: is_final,
                    },
                );
            }

            current = &mut current.get_mut(part).expect("just inserted").children;
        }
    }

    tree_to_nav_nodes(root, String::new())
}

// Sidebar nav: every page in the index, title-sorted, for the explorer list
// rendered by base.html. The index is the disposable read model; a freshly
// saved page appears once the background indexer catches up.
async fn nav_pages(db: &sqlx::PgPool) -> Result<Vec<NavNode>, AppError> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT path, title FROM tb_pages ORDER BY title")
            .fetch_all(db)
            .await
            .context("Failed to load nav pages")?;
    let stripped_rows: Vec<(String, String)> = rows
        .into_iter()
        .map(|(path, title)| (path.strip_suffix(".md").unwrap_or(&path).to_string(), title))
        .collect();
    Ok(build_nav_tree(stripped_rows))
}

// Dispatch to view or edit based on the path suffix
async fn page_handler(
    Path(path): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    if let Some(stripped_path) = path.strip_suffix("/edit") {
        page_edit(stripped_path.to_string(), state).await
    } else {
        page_view(path, state).await
    }
}

// Render the read-only page view
async fn page_view(path: String, state: AppState) -> Result<Response, AppError> {
    info!("Rendering page view for path: {}", path);
    let file_path = safe_file_path(&path)?;
    let template = state.templates.get_template("page.html")?;
    let nav = nav_pages(&state.db).await?;

    if !file_path.exists() {
        let rendered = template.render(context! {
            title => format!("Create Page: {path}"),
            path => path,
            exists => false,
            content_html => "",
            loaded_hash => "",
            has_mermaid => false,
            backlinks => Vec::<Backlink>::new(),
            nav_pages => nav,
        })?;
        return Ok(Html(rendered).into_response());
    }

    let raw_content = fs::read_to_string(&file_path)
        .context(format!("Failed to read file: {}", file_path.display()))?;
    let loaded_hash = compute_hash(&raw_content);
    let (frontmatter, body) = parse_frontmatter(&raw_content);
    let title = extract_title(&path, frontmatter.as_ref(), body);

    // Resolve wikilink targets against the index so missing pages render
    // distinctly. The index is a disposable read model; a freshly saved page
    // may briefly resolve as missing until the background indexer catches up.
    let slugs: Vec<(String,)> = sqlx::query_as("SELECT slug FROM tb_pages")
        .fetch_all(&state.db)
        .await
        .context("Failed to load page slugs for wikilink resolution")?;
    let slug_set: HashSet<String> = slugs.into_iter().map(|(s,)| s).collect();

    let content_html = render_html(body, &|norm| slug_set.contains(norm));

    // Check has_mermaid
    let has_mermaid = raw_content.contains("```mermaid");

    // Load backlinks: pages that link TO this page
    let page_id_result: Option<(i64,)> = sqlx::query_as("SELECT id FROM tb_pages WHERE path = $1")
        .bind(format!("{path}.md"))
        .fetch_optional(&state.db)
        .await
        .context("Failed to query page id for backlinks")?;

    let backlinks = if let Some((page_id,)) = page_id_result {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT src.path, src.title
             FROM tb_links l
             JOIN tb_pages src ON src.id = l.src_id
             WHERE l.target_id = $1 AND l.kind = 'page'
             ORDER BY src.title
             LIMIT 50",
        )
        .bind(page_id)
        .fetch_all(&state.db)
        .await
        .context("Failed to load backlinks")?;

        rows.into_iter()
            .map(|(p, t)| Backlink {
                path: p.strip_suffix(".md").unwrap_or(&p).to_string(),
                title: t,
            })
            .collect()
    } else {
        Vec::new()
    };

    let rendered = template.render(context! {
        title => title,
        path => path,
        exists => true,
        content_html => content_html,
        loaded_hash => loaded_hash,
        has_mermaid => has_mermaid,
        backlinks => backlinks,
        nav_pages => nav,
    })?;

    Ok(Html(rendered).into_response())
}

// Render the edit page
async fn page_edit(path: String, state: AppState) -> Result<Response, AppError> {
    info!("Rendering edit page for path: {}", path);
    let file_path = safe_file_path(&path)?;
    let template = state.templates.get_template("edit.html")?;

    let (body, loaded_hash) = if file_path.exists() {
        let raw_content = fs::read_to_string(&file_path)
            .context(format!("Failed to read file: {}", file_path.display()))?;
        let hash = compute_hash(&raw_content);
        (raw_content, hash)
    } else {
        (String::new(), String::new())
    };

    let nav = nav_pages(&state.db).await?;
    let rendered = template.render(context! {
        path => path,
        body => body,
        loaded_hash => loaded_hash,
        nav_pages => nav,
    })?;

    Ok(Html(rendered).into_response())
}

#[derive(serde::Deserialize)]
struct EditForm {
    body: String,
    loaded_hash: String,
}

// Handle the saving of a page
async fn page_save(
    Path(path): Path<String>,
    State(state): State<AppState>,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    info!("Saving page path: {}", path);
    let file_path = safe_file_path(&path)?;

    // If file exists, do optimistic concurrency check
    if file_path.exists() {
        let disk_content = fs::read_to_string(&file_path).context(format!(
            "Failed to read file for hash check: {}",
            file_path.display()
        ))?;
        let disk_hash = compute_hash(&disk_content);

        if disk_hash != form.loaded_hash {
            warn!("Conflict detected on page save: path={}", path);
            let template = state.templates.get_template("conflict.html")?;
            let nav = nav_pages(&state.db).await?;
            let rendered = template.render(context! {
                path => path,
                current_content => disk_content,
                submitted_content => form.body,
                current_hash => disk_hash,
                nav_pages => nav,
            })?;
            return Ok((StatusCode::CONFLICT, Html(rendered)).into_response());
        }
    }

    // Atomic write
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).context(format!(
            "Failed to create parent directories: {}",
            parent.display()
        ))?;
    }

    let temp_path = file_path.with_extension("tmp");
    {
        let mut file = fs::File::create(&temp_path).context(format!(
            "Failed to create temp file: {}",
            temp_path.display()
        ))?;
        file.write_all(form.body.as_bytes())
            .context("Failed to write to temp file")?;
        file.sync_all()
            .context("Failed to sync temp file to disk")?;
    }

    fs::rename(&temp_path, &file_path).context(format!(
        "Failed to rename temp file to target: {}",
        file_path.display()
    ))?;

    info!("Saved page path={} successfully", path);
    Ok(Redirect::to(&format!("/p/{path}")).into_response())
}

#[derive(serde::Deserialize)]
struct MoveForm {
    from: String,
    to: String,
}

// Handle moving/renaming a page
async fn page_move(Form(form): Form<MoveForm>) -> Result<Response, AppError> {
    info!("Moving page from: {} to: {}", form.from, form.to);
    let src = safe_file_path(&form.from)?;
    let dst = safe_file_path(&form.to)?;

    if !src.exists() {
        return Err(anyhow::anyhow!("Source page not found: {}", form.from).into());
    }

    if dst.exists() {
        return Ok((StatusCode::CONFLICT, "Target page already exists").into_response());
    }

    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).context(format!(
            "Failed to create parent directories: {}",
            parent.display()
        ))?;
    }

    fs::rename(&src, &dst).context(format!(
        "Failed to move file from {} to {}",
        src.display(),
        dst.display()
    ))?;

    info!("Moved page from {} to {} successfully", form.from, form.to);
    Ok(Redirect::to(&format!("/p/{}", form.to)).into_response())
}

#[derive(serde::Deserialize)]
struct TrashForm {
    path: String,
}

// Handle trashing a page
async fn page_trash(Form(form): Form<TrashForm>) -> Result<Response, AppError> {
    info!("Trashing page: {}", form.path);
    let src = safe_file_path(&form.path)?;

    if !src.exists() {
        return Err(anyhow::anyhow!("Page not found: {}", form.path).into());
    }

    let trash_dir = StdPath::new("miku").join(".trash");
    fs::create_dir_all(&trash_dir).context(format!(
        "Failed to create trash directory: {}",
        trash_dir.display()
    ))?;

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("Failed to get current time")?
        .as_secs();
    let flattened = form.path.replace('/', "-");
    let trash_filename = format!("{flattened}-{ts}.md");
    let trash_dst = trash_dir.join(&trash_filename);

    fs::rename(&src, &trash_dst).context(format!(
        "Failed to move file to trash: {}",
        trash_dst.display()
    ))?;

    info!("Trashed page {} to {}", form.path, trash_filename);
    Ok(Redirect::to("/p/Index").into_response())
}

// Search handler: full-text search over pages
#[derive(serde::Deserialize)]
struct SearchParams {
    q: Option<String>,
}

async fn search(
    Query(params): Query<SearchParams>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    info!("Rendering search");
    let template = state.templates.get_template("search.html")?;

    let query_str = params.q.as_deref().unwrap_or("").trim().to_string();

    let results = if query_str.is_empty() {
        Vec::new()
    } else {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT path, title
             FROM tb_pages
             WHERE body_tsv @@ websearch_to_tsquery('english', $1)
             ORDER BY ts_rank(body_tsv, websearch_to_tsquery('english', $1)) DESC
             LIMIT 50",
        )
        .bind(&query_str)
        .fetch_all(&state.db)
        .await
        .context("Failed to execute full-text search")?;

        rows.into_iter()
            .map(|(path, title)| PageRef {
                path: path.strip_suffix(".md").unwrap_or(&path).to_string(),
                title,
            })
            .collect()
    };

    let nav = nav_pages(&state.db).await?;
    let rendered = template.render(context! {
        query => query_str,
        results => results,
        nav_pages => nav,
        section => "search",
    })?;

    Ok(Html(rendered).into_response())
}

// Tags index handler: list all tags with their counts
async fn tags_index(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    info!("Rendering tags index");
    let template = state.templates.get_template("tags.html")?;

    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT tag, COUNT(*) AS cnt FROM tb_tags GROUP BY tag ORDER BY cnt DESC, tag",
    )
    .fetch_all(&state.db)
    .await
    .context("Failed to load tags")?;

    let tags: Vec<TagCount> = rows
        .into_iter()
        .map(|(tag, count)| TagCount { tag, count })
        .collect();

    let nav = nav_pages(&state.db).await?;
    let rendered = template.render(context! {
        tags => tags,
        nav_pages => nav,
        section => "tags",
    })?;

    Ok(Html(rendered).into_response())
}

// Tag filter handler: list all pages with a specific tag
async fn tag_filter(
    Path(tag): Path<String>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    info!("Rendering tag filter for tag: {}", tag);
    let template = state.templates.get_template("tag.html")?;

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.path, p.title
         FROM tb_tags t JOIN tb_pages p ON p.id = t.page_id
         WHERE t.tag = $1 ORDER BY p.title",
    )
    .bind(&tag)
    .fetch_all(&state.db)
    .await
    .context("Failed to load pages for tag")?;

    let pages: Vec<PageRef> = rows
        .into_iter()
        .map(|(path, title)| PageRef {
            path: path.strip_suffix(".md").unwrap_or(&path).to_string(),
            title,
        })
        .collect();

    let nav = nav_pages(&state.db).await?;
    let rendered = template.render(context! {
        tag => tag,
        pages => pages,
        nav_pages => nav,
        section => "tags",
    })?;

    Ok(Html(rendered).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_rendering() {
        let mut templates_env = Environment::new();
        templates_env.set_loader(minijinja::path_loader("src/templates"));

        let template = templates_env
            .get_template("page.html")
            .expect("Failed to get page.html template");
        let rendered = template
            .render(context! {
                title => "Test Title",
                path => "TestPath",
                exists => true,
                content_html => "<p>Test content</p>",
                loaded_hash => "abc",
                has_mermaid => false,
            })
            .expect("Failed to render template");

        assert!(rendered.contains("Test Title"));
        assert!(rendered.contains("miku/TestPath.md"));
        assert!(!rendered.contains("mermaid.min.js"));
    }

    #[test]
    fn test_template_rendering_with_mermaid() {
        let mut templates_env = Environment::new();
        templates_env.set_loader(minijinja::path_loader("src/templates"));

        let template = templates_env
            .get_template("page.html")
            .expect("Failed to get page.html template");
        let rendered = template
            .render(context! {
                title => "Test Title",
                path => "TestPath",
                exists => true,
                content_html => "<p>Test content</p>",
                loaded_hash => "abc",
                has_mermaid => true,
            })
            .expect("Failed to render template");

        assert!(rendered.contains("mermaid.min.js"));
    }

    #[test]
    fn test_build_nav_tree_nested_structure() {
        let rows = vec![
            ("a".to_string(), "A".to_string()),
            ("b/c".to_string(), "C".to_string()),
            ("b/d".to_string(), "D".to_string()),
        ];
        let result = build_nav_tree(rows);

        // Folders first, then pages
        assert_eq!(result.len(), 2);

        // First should be folder "b" (folders come first)
        assert_eq!(result[0].name, "b");
        assert_eq!(result[0].path, None);
        assert_eq!(result[0].children.len(), 2);

        // Folder b's children should be sorted: c, d (both pages)
        assert_eq!(result[0].children[0].name, "C");
        assert_eq!(result[0].children[0].path, Some("b/c".to_string()));
        assert_eq!(result[0].children[0].children.len(), 0);

        assert_eq!(result[0].children[1].name, "D");
        assert_eq!(result[0].children[1].path, Some("b/d".to_string()));
        assert_eq!(result[0].children[1].children.len(), 0);

        // Second should be page "a" (pages come after folders)
        assert_eq!(result[1].name, "A");
        assert_eq!(result[1].path, Some("a".to_string()));
        assert_eq!(result[1].children.len(), 0);
    }

    #[test]
    fn test_build_nav_tree_leaf_uses_title() {
        let rows = vec![("mypage".to_string(), "My Page Title".to_string())];
        let result = build_nav_tree(rows);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "My Page Title");
        assert_eq!(result[0].path, Some("mypage".to_string()));
    }

    #[test]
    fn test_build_nav_tree_folder_uses_segment() {
        let rows = vec![
            ("docs/api".to_string(), "API Reference".to_string()),
            ("docs/guide".to_string(), "User Guide".to_string()),
        ];
        let result = build_nav_tree(rows);

        // Root should have one folder
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "docs");
        assert_eq!(result[0].path, None);
        assert_eq!(result[0].children.len(), 2);

        // Children should be sorted alphabetically by name (case-insensitive)
        assert_eq!(result[0].children[0].name, "API Reference");
        assert_eq!(result[0].children[1].name, "User Guide");
    }

    #[test]
    fn test_build_nav_tree_sorting_case_insensitive() {
        let rows = vec![
            ("zebra".to_string(), "Zebra".to_string()),
            ("apple".to_string(), "Apple".to_string()),
            ("Banana".to_string(), "Banana".to_string()),
        ];
        let result = build_nav_tree(rows);

        // Should be sorted case-insensitively
        assert_eq!(result[0].name, "Apple");
        assert_eq!(result[1].name, "Banana");
        assert_eq!(result[2].name, "Zebra");
    }

    #[test]
    fn test_build_nav_tree_empty() {
        let rows = vec![];
        let result = build_nav_tree(rows);

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_build_nav_tree_deep_nesting() {
        let rows = vec![
            ("a/b/c/d".to_string(), "Deep Page".to_string()),
            ("a/b/e".to_string(), "E".to_string()),
        ];
        let result = build_nav_tree(rows);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "a");
        assert_eq!(result[0].path, None);

        let level1 = &result[0].children;
        assert_eq!(level1.len(), 1);
        assert_eq!(level1[0].name, "b");
        assert_eq!(level1[0].path, None);

        let level2 = &level1[0].children;
        assert_eq!(level2.len(), 2);
        // c folder should come before e page
        assert_eq!(level2[0].name, "c");
        assert_eq!(level2[0].path, None);
        assert_eq!(level2[1].name, "E");
        assert_eq!(level2[1].path, Some("a/b/e".to_string()));

        let level3 = &level2[0].children;
        assert_eq!(level3.len(), 1);
        assert_eq!(level3[0].name, "Deep Page");
        assert_eq!(level3[0].path, Some("a/b/c/d".to_string()));
    }

    #[test]
    fn test_safe_file_path_rejects_traversal() {
        let result = safe_file_path("../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_safe_file_path_rejects_absolute() {
        let result = safe_file_path("/abs");
        assert!(result.is_err());
    }
}
