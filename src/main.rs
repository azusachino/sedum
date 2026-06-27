use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{
        sse::{self, KeepAlive, Sse},
        Html, IntoResponse, Redirect, Response,
    },
    routing::{get, post},
    Form, Router,
};
use chrono::{DateTime, Local};
use miku::markdown::{extract_title, parse_frontmatter, render_html_with_toc, Heading};
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
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};
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
struct SearchResult {
    path: String,
    title: String,
    snippet: String,
}

fn search_snippet(body: &str, query: &str) -> String {
    let plain = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.is_empty() {
        return "No preview available.".to_string();
    }

    let needle = query
        .split_whitespace()
        .find(|word| word.chars().any(char::is_alphanumeric))
        .unwrap_or(query)
        .to_ascii_lowercase();
    let plain_lower = plain.to_ascii_lowercase();
    let match_at = plain_lower.find(&needle).unwrap_or(0);
    let start = plain[..match_at]
        .rfind(' ')
        .and_then(|idx| plain[..idx].rfind(' '))
        .and_then(|idx| plain[..idx].rfind(' '))
        .unwrap_or(0);
    let end = plain[match_at..]
        .char_indices()
        .filter(|(_, ch)| ch.is_whitespace())
        .nth(30)
        .map(|(idx, _)| match_at + idx)
        .unwrap_or(plain.len());
    let snippet = plain[start..end].trim();

    if end < plain.len() {
        format!("{snippet}...")
    } else {
        snippet.to_string()
    }
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
    // Broadcasts the relative path (`.md` stripped) of each page the background
    // indexer just re-indexed, so connected browsers can live-refresh via SSE.
    // Read-only fan-out: the SSE layer never writes the Postgres index.
    events: tokio::sync::broadcast::Sender<String>,
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

    // SSE broadcast channel: the indexer is the sole sender; each browser /events
    // connection is a subscriber. Capacity 256 bounds backpressure; slow
    // subscribers see Lagged and resync on the next event.
    let (events_tx, _) = tokio::sync::broadcast::channel::<String>(256);

    let state = AppState {
        db: pool.clone(),
        templates: Arc::new(templates_env),
        events: events_tx.clone(),
    };

    // 6. Initialize background indexer
    let _indexer =
        miku::indexer::IndexerQueue::new(pool, std::path::PathBuf::from("miku"), events_tx)
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
        .route("/preview", post(preview))
        .route("/p/{*path}", get(page_handler).post(page_save))
        .route("/events", get(events))
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

// Server-Sent Events stream of re-indexed page paths. One-way server->client:
// the browser opens `new EventSource('/events')` and live-refreshes the open
// page when it sees its own path. This handler only SUBSCRIBES to the broadcast
// channel filled by the background indexer; it never writes the Postgres index,
// preserving the single-writer invariant.
async fn events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<sse::Event, std::convert::Infallible>>> {
    let rx = state.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| {
        // Drop Lagged errors gracefully: the client refetches on the next event.
        item.ok().map(|path| Ok(sse::Event::default().data(path)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
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

fn format_modified_time(file_path: &StdPath) -> String {
    fs::metadata(file_path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(|modified| {
            let local: DateTime<Local> = modified.into();
            local.format("%Y-%m-%d %H:%M").to_string()
        })
        .unwrap_or_else(|| "Unknown".to_string())
}

fn breadcrumb_parent(path: &str) -> Option<String> {
    path.rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .filter(|parent| !parent.is_empty())
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
    Query(params): Query<EditQuery>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    if let Some(stripped_path) = path.strip_suffix("/edit") {
        page_edit(stripped_path.to_string(), params.template, state).await
    } else {
        page_view(path, state).await
    }
}

// Optional `?template=<id>` from the create-page modal, used to seed a brand-new
// page's editor body. Ignored for existing pages.
#[derive(serde::Deserialize)]
struct EditQuery {
    template: Option<String>,
}

// Seed bodies for the create-page modal's "start from" templates. The server is
// the single source of truth for this content (the modal only passes the id),
// so a freshly created page opens prefilled without a client-side markdown lib.
fn template_seed(id: &str) -> &'static str {
    match id {
        "meeting" => "# Meeting\n\n## Agenda\n\n## Notes\n\n## Actions\n",
        "reading" => "# Reading Notes\n\n## Summary\n\n## Highlights\n\n## Questions\n",
        "project" => "# Project\n\n## Goal\n\n## Tasks\n\n## Status\n",
        _ => "",
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
            toc => Vec::<Heading>::new(),
            word_count => 0usize,
            backlink_count => 0usize,
            updated => "Missing",
            frontmatter => serde_json::Value::Object(serde_json::Map::new()),
            breadcrumb_parent => breadcrumb_parent(&path),
            nav_pages => nav,
        })?;
        return Ok(Html(rendered).into_response());
    }

    let raw_content = fs::read_to_string(&file_path)
        .context(format!("Failed to read file: {}", file_path.display()))?;
    let loaded_hash = compute_hash(&raw_content);
    let (frontmatter, body) = parse_frontmatter(&raw_content);
    let title = extract_title(&path, frontmatter.as_ref(), body);
    let word_count = body.split_whitespace().count();
    let updated = format_modified_time(&file_path);

    // Resolve wikilink targets against the index so missing pages render
    // distinctly. The index is a disposable read model; a freshly saved page
    // may briefly resolve as missing until the background indexer catches up.
    let slugs: Vec<(String,)> = sqlx::query_as("SELECT slug FROM tb_pages")
        .fetch_all(&state.db)
        .await
        .context("Failed to load page slugs for wikilink resolution")?;
    let slug_set: HashSet<String> = slugs.into_iter().map(|(s,)| s).collect();

    let (content_html, toc) = render_html_with_toc(body, &|norm| slug_set.contains(norm));

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
    let backlink_count = backlinks.len();
    let frontmatter =
        frontmatter.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));

    let rendered = template.render(context! {
        title => title,
        path => path,
        exists => true,
        content_html => content_html,
        loaded_hash => loaded_hash,
        has_mermaid => has_mermaid,
        backlinks => backlinks,
        toc => toc,
        word_count => word_count,
        backlink_count => backlink_count,
        updated => updated,
        frontmatter => frontmatter,
        breadcrumb_parent => breadcrumb_parent(&path),
        nav_pages => nav,
    })?;

    Ok(Html(rendered).into_response())
}

// Render the edit page
async fn page_edit(
    path: String,
    template_id: Option<String>,
    state: AppState,
) -> Result<Response, AppError> {
    info!("Rendering edit page for path: {}", path);
    let file_path = safe_file_path(&path)?;
    let template = state.templates.get_template("edit.html")?;

    let (body, loaded_hash) = if file_path.exists() {
        let raw_content = fs::read_to_string(&file_path)
            .context(format!("Failed to read file: {}", file_path.display()))?;
        let hash = compute_hash(&raw_content);
        (raw_content, hash)
    } else {
        // New page: seed the editor from the chosen create-modal template (if
        // any). loaded_hash stays empty so the save path treats it as a create.
        let seed = template_id.as_deref().map(template_seed).unwrap_or("");
        (seed.to_string(), String::new())
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

#[derive(serde::Deserialize)]
struct PreviewForm {
    body: String,
}

async fn preview(
    State(state): State<AppState>,
    Form(form): Form<PreviewForm>,
) -> Result<impl IntoResponse, AppError> {
    let slugs: Vec<(String,)> = sqlx::query_as("SELECT slug FROM tb_pages")
        .fetch_all(&state.db)
        .await
        .context("Failed to load page slugs for preview wikilink resolution")?;
    let slug_set: HashSet<String> = slugs.into_iter().map(|(s,)| s).collect();
    let (_, body) = parse_frontmatter(&form.body);
    let (content_html, _) = render_html_with_toc(body, &|norm| slug_set.contains(norm));

    Ok(Html(content_html))
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
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT path, title, body
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
            .map(|(path, title, body)| {
                let snippet = search_snippet(&body, &query_str);

                SearchResult {
                    path: path.strip_suffix(".md").unwrap_or(&path).to_string(),
                    title,
                    snippet,
                }
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
                backlinks => Vec::<Backlink>::new(),
                toc => Vec::<Heading>::new(),
                word_count => 2usize,
                backlink_count => 0usize,
                updated => "2026-06-27 12:00",
                frontmatter => serde_json::Value::Object(serde_json::Map::new()),
                breadcrumb_parent => Option::<String>::None,
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
                backlinks => Vec::<Backlink>::new(),
                toc => Vec::<Heading>::new(),
                word_count => 2usize,
                backlink_count => 0usize,
                updated => "2026-06-27 12:00",
                frontmatter => serde_json::Value::Object(serde_json::Map::new()),
                breadcrumb_parent => Option::<String>::None,
            })
            .expect("Failed to render template");

        assert!(rendered.contains("mermaid.min.js"));
    }

    #[test]
    fn test_template_seed_maps_ids_to_bodies() {
        assert!(template_seed("meeting").contains("## Agenda"));
        assert!(template_seed("reading").contains("## Highlights"));
        assert!(template_seed("project").contains("## Tasks"));
        // Blank and unknown ids both produce an empty page.
        assert_eq!(template_seed("blank"), "");
        assert_eq!(template_seed("bogus"), "");
    }

    #[test]
    fn test_edit_template_renders_seed_body_into_textarea() {
        let mut templates_env = Environment::new();
        templates_env.set_loader(minijinja::path_loader("src/templates"));

        let template = templates_env
            .get_template("edit.html")
            .expect("Failed to get edit.html template");
        // Mirrors page_edit seeding a new page from ?template=meeting.
        let rendered = template
            .render(context! {
                path => "Notes/Standup",
                body => template_seed("meeting"),
                loaded_hash => "",
                nav_pages => Vec::<NavNode>::new(),
            })
            .expect("Failed to render template");

        assert!(rendered.contains("## Agenda"));
        assert!(rendered.contains("## Actions"));
    }

    #[test]
    fn test_edit_template_has_live_preview_editor() {
        let mut templates_env = Environment::new();
        templates_env.set_loader(minijinja::path_loader("src/templates"));

        let template = templates_env
            .get_template("edit.html")
            .expect("Failed to get edit.html template");
        let rendered = template
            .render(context! {
                path => "TestPath",
                body => "# Draft",
                loaded_hash => "abc",
                nav_pages => Vec::<NavNode>::new(),
            })
            .expect("Failed to render template");

        assert!(rendered.contains("class=\"mk-edit\""));
        assert!(rendered.contains("class=\"mk-edit-split\""));
        assert!(rendered.contains("class=\"mk-preview mk-prose\""));
        assert!(rendered.contains("name=\"loaded_hash\" value=\"abc\""));
        assert!(rendered.contains("fetch('/preview'"));
        assert!(rendered.contains("action=\"/p/TestPath\" method=\"POST\""));
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

    // The SSE feature is a read-only broadcast fan-out: the indexer sends a
    // page path, every subscriber's stream yields it. This proves the
    // broadcast -> BroadcastStream wiring in isolation (no DB, no HTTP server),
    // mirroring exactly what the `/events` handler does internally.
    #[tokio::test]
    async fn test_events_broadcast_reaches_subscriber_stream() {
        let (tx, _) = tokio::sync::broadcast::channel::<String>(256);

        // Subscribe BEFORE sending (mirrors a connected browser).
        let rx = tx.subscribe();
        let mut stream = BroadcastStream::new(rx)
            .filter_map(|item| item.ok().map(Ok::<_, std::convert::Infallible>));

        // The indexer broadcasts a re-indexed page path (`.md` stripped form).
        tx.send("Notes/Daily".to_string())
            .expect("subscriber present");

        let received = stream.next().await.expect("stream item").expect("ok item");
        assert_eq!(received, "Notes/Daily");
    }

    // `send` returns Err only when there are no subscribers; the indexer ignores
    // that with `let _ =`. Confirm the no-subscriber case is an error (so the
    // ignore is correct) and does not panic.
    #[test]
    fn test_events_send_with_no_subscribers_is_err() {
        let (tx, rx) = tokio::sync::broadcast::channel::<String>(256);
        drop(rx);
        assert!(tx.send("Orphan".to_string()).is_err());
    }

    #[tokio::test]
    async fn test_app_router_registers_events_route() {
        // Build the router with a dummy AppState (no DB connection is made until
        // a handler runs). This proves `/events` is wired into `fn app`.
        let (events, _) = tokio::sync::broadcast::channel::<String>(256);
        let mut templates_env = Environment::new();
        templates_env.set_loader(minijinja::path_loader("src/templates"));
        let state = AppState {
            db: sqlx::PgPool::connect_lazy("postgres://localhost/miku_test_unused")
                .expect("lazy pool"),
            templates: Arc::new(templates_env),
            events,
        };
        // If `/events` (or any route) were malformed, `app` would panic here.
        let _router = app(state);
    }

    #[test]
    fn test_page_template_has_sse_live_preview() {
        let mut templates_env = Environment::new();
        templates_env.set_loader(minijinja::path_loader("src/templates"));

        let template = templates_env
            .get_template("page.html")
            .expect("Failed to get page.html template");
        let rendered = template
            .render(context! {
                title => "Test Title",
                path => "Notes/Daily",
                exists => true,
                content_html => "<p>Test content</p>",
                loaded_hash => "abc",
                has_mermaid => false,
                backlinks => Vec::<Backlink>::new(),
                toc => Vec::<Heading>::new(),
                word_count => 2usize,
                backlink_count => 0usize,
                updated => "2026-06-27 12:00",
                frontmatter => serde_json::Value::Object(serde_json::Map::new()),
                breadcrumb_parent => Option::<String>::None,
            })
            .expect("Failed to render template");

        // minijinja HTML-escapes `/` to `&#x2f;` inside the attribute value; the
        // browser's getAttribute decodes it back to "Notes/Daily", matching the
        // unescaped path the SSE broadcast sends. Assert on the escaped form.
        assert!(rendered.contains("data-page-path=\"Notes&#x2f;Daily\""));
        assert!(rendered.contains("new EventSource(\"/events\")"));
        assert!(rendered.contains("class=\"mk-synced\""));
        assert!(rendered.contains("data-sync-indicator"));
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
