use anyhow::{Context, Result};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Form, Router,
};
use minijinja::{context, Environment};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::env;
use std::fs;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    #[allow(dead_code)]
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
                tracing_subscriber::EnvFilter::new("info,sedum=debug,tower_http=debug")
            }),
        )
        .init();

    info!("Starting Sedum Server...");

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
    let _indexer = sedum::indexer::IndexerQueue::new(pool, std::path::PathBuf::from("sedum"))
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
        .route("/p/{*path}", get(page_handler).post(page_save))
        .nest_service("/static", ServeDir::new("static"))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

// Redirect root "/" to "/p/Index"
async fn redirect_to_index() -> impl IntoResponse {
    Redirect::temporary("/p/Index")
}

// Helper to get safe path under sedum/ and check for directory traversal
fn safe_file_path(path: &str) -> Result<PathBuf, AppError> {
    if path.contains("..") || path.starts_with('/') {
        return Err(anyhow::anyhow!("Invalid path: path traversal detected").into());
    }
    Ok(StdPath::new("sedum").join(format!("{path}.md")))
}

// Helper to compute SHA-256 hash of content
fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

// Helper to parse frontmatter and body
fn parse_markdown(content: &str) -> (Option<serde_yaml::Value>, &str) {
    if content.starts_with("---\n") || content.starts_with("---\r\n") {
        let header_len = if content.starts_with("---\n") { 4 } else { 5 };
        let rest = &content[header_len..];
        let mut search_idx = 0;
        while let Some(idx) = rest[search_idx..].find("\n---") {
            let actual_idx = search_idx + idx;
            let after = &rest[actual_idx + 4..];
            if after.is_empty() || after.starts_with('\n') || after.starts_with('\r') {
                let yaml_str = &rest[..actual_idx];
                let after_first_nl = after.find('\n').map_or(0, |n| n + 1);
                let body_str = &after[after_first_nl..];
                if let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(yaml_str) {
                    return (Some(yaml), body_str);
                }
                break;
            }
            search_idx = actual_idx + 4;
        }
    }
    (None, content)
}

// Extract title from frontmatter, H1 header, or default to path basename
fn extract_title(path: &str, frontmatter: Option<&serde_yaml::Value>, body: &str) -> String {
    if let Some(fm) = frontmatter {
        if let Some(title) = fm.get("title").and_then(|t| t.as_str()) {
            return title.to_string();
        }
    }

    // Fallback to first H1
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            return stripped.trim().to_string();
        }
    }

    // Fallback to basename
    StdPath::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
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

    if !file_path.exists() {
        let rendered = template.render(context! {
            title => format!("Create Page: {path}"),
            path => path,
            exists => false,
            content_html => "",
            loaded_hash => "",
            has_mermaid => false,
        })?;
        return Ok(Html(rendered).into_response());
    }

    let raw_content = fs::read_to_string(&file_path)
        .context(format!("Failed to read file: {}", file_path.display()))?;
    let loaded_hash = compute_hash(&raw_content);
    let (frontmatter, body) = parse_markdown(&raw_content);
    let title = extract_title(&path, frontmatter.as_ref(), body);

    // Setup comrak options
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    options.extension.alerts = true;
    options.extension.wikilinks_title_after_pipe = true;

    let content_html = comrak::markdown_to_html(body, &options);

    // Check has_mermaid
    let has_mermaid = raw_content.contains("```mermaid");

    let rendered = template.render(context! {
        title => title,
        path => path,
        exists => true,
        content_html => content_html,
        loaded_hash => loaded_hash,
        has_mermaid => has_mermaid,
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

    let rendered = template.render(context! {
        path => path,
        body => body,
        loaded_hash => loaded_hash,
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
            let rendered = template.render(context! {
                path => path,
                current_content => disk_content,
                submitted_content => form.body,
                current_hash => disk_hash,
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
        assert!(rendered.contains("sedum/TestPath.md"));
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
    fn test_parse_markdown_frontmatter() {
        let content = "---\ntitle: Hello World\n---\n# Header\nBody content";
        let (yaml, body) = parse_markdown(content);
        assert!(yaml.is_some());
        let val = yaml.unwrap();
        assert_eq!(
            val.get("title").and_then(|t| t.as_str()),
            Some("Hello World")
        );
        assert_eq!(body, "# Header\nBody content");
    }

    #[test]
    fn test_parse_markdown_no_frontmatter() {
        let content = "# Header\nBody content";
        let (yaml, body) = parse_markdown(content);
        assert!(yaml.is_none());
        assert_eq!(body, "# Header\nBody content");
    }
}
