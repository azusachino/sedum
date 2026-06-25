use anyhow::Result;
use comrak::nodes::NodeValue;
use notify::Watcher;
use regex::Regex;
use sqlx::PgPool;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{error, info, warn};

// Import normalize_slug and other helpers if needed, or define here.
fn normalize_target(name: &str, is_asset: bool) -> String {
    let trimmed = name.trim();
    if is_asset {
        trimmed.to_lowercase()
    } else {
        let stripped = if trimmed.to_lowercase().ends_with(".md") {
            &trimmed[..trimmed.len() - 3]
        } else {
            trimmed
        };
        stripped.to_lowercase()
    }
}

fn is_asset_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".svg")
        || lower.ends_with(".pdf")
        || lower.ends_with(".webp")
}

fn extract_tags_from_frontmatter(fm: &serde_json::Value) -> HashSet<String> {
    let mut tags = HashSet::new();
    if let Some(tags_val) = fm.get("tags") {
        if let Some(arr) = tags_val.as_array() {
            for v in arr {
                if let Some(s) = v.as_str() {
                    tags.insert(s.trim().to_string());
                }
            }
        } else if let Some(s) = tags_val.as_str() {
            for t in s.split(',') {
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    tags.insert(trimmed.to_string());
                }
            }
        }
    }
    tags
}

fn extract_aliases(fm: &serde_json::Value) -> HashSet<String> {
    let mut aliases = HashSet::new();
    if let Some(val) = fm.get("aliases") {
        if let Some(arr) = val.as_array() {
            for v in arr {
                if let Some(s) = v.as_str() {
                    aliases.insert(s.trim().to_string());
                }
            }
        } else if let Some(s) = val.as_str() {
            for a in s.split(',') {
                let trimmed = a.trim();
                if !trimmed.is_empty() {
                    aliases.insert(trimmed.to_string());
                }
            }
        }
    }
    aliases
}

// Parses frontmatter using serde_yaml
fn parse_markdown(content: &str) -> (Option<serde_json::Value>, &str) {
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
                if let Ok(yaml) = serde_yaml::from_str::<serde_json::Value>(yaml_str) {
                    return (Some(yaml), body_str);
                }
                break;
            }
            search_idx = actual_idx + 4;
        }
    }
    (None, content)
}

fn extract_title(path: &str, frontmatter: Option<&serde_json::Value>, body: &str) -> String {
    if let Some(fm) = frontmatter {
        if let Some(title) = fm.get("title").and_then(|t| t.as_str()) {
            return title.to_string();
        }
    }

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(stripped) = trimmed.strip_prefix("# ") {
            return stripped.trim().to_string();
        }
    }

    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
        .to_string()
}

struct ExtractedLink {
    kind: String,
    is_embed: bool,
    target: String,
    target_norm: String,
    alias: Option<String>,
}

struct PageMetadata {
    title: String,
    has_mermaid: bool,
    tags: HashSet<String>,
    links: Vec<ExtractedLink>,
    aliases: HashSet<String>,
}

fn extract_metadata(
    path: &str,
    frontmatter: Option<&serde_json::Value>,
    body: &str,
) -> PageMetadata {
    let arena = comrak::Arena::new();
    let mut options = comrak::Options::default();
    options.extension.wikilinks_title_after_pipe = true;
    let root = comrak::parse_document(&arena, body, &options);

    let mut tags = frontmatter
        .map(extract_tags_from_frontmatter)
        .unwrap_or_default();
    let aliases = frontmatter.map(extract_aliases).unwrap_or_default();
    let mut links = Vec::new();

    let has_mermaid = body.contains("```mermaid");

    lazy_static::lazy_static! {
        static ref EMBED_REGEX: Regex = Regex::new(r"!\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").unwrap();
        static ref TAG_REGEX: Regex = Regex::new(r"(?:\s|^|['`\(])#([\p{L}\p{N}][\p{L}\p{N}_\-/]*)").unwrap();
    }

    let is_skipped = |node: &comrak::nodes::AstNode| -> bool {
        let mut current = node.parent();
        while let Some(parent) = current {
            let val = &parent.data.borrow().value;
            match val {
                NodeValue::Heading(_) | NodeValue::CodeBlock(_) | NodeValue::Link(_) => {
                    return true
                }
                _ => {}
            }
            current = parent.parent();
        }
        false
    };

    for node in root.descendants() {
        if is_skipped(node) {
            continue;
        }

        match &node.data.borrow().value {
            NodeValue::WikiLink(w) => {
                let target = w.url.clone();
                let target_norm = normalize_target(&target, false);

                let mut alias_text = String::new();
                for child in node.children() {
                    for desc in child.descendants() {
                        if let NodeValue::Text(t) = &desc.data.borrow().value {
                            alias_text.push_str(t);
                        }
                    }
                }

                let alias = if alias_text.is_empty() || alias_text == target {
                    None
                } else {
                    Some(alias_text)
                };

                links.push(ExtractedLink {
                    kind: "page".to_string(),
                    is_embed: false,
                    target,
                    target_norm,
                    alias,
                });
            }
            NodeValue::Text(t) => {
                for cap in TAG_REGEX.captures_iter(t) {
                    if let Some(m) = cap.get(1) {
                        tags.insert(m.as_str().to_string());
                    }
                }

                for cap in EMBED_REGEX.captures_iter(t) {
                    if let Some(target_match) = cap.get(1) {
                        let target = target_match.as_str().trim().to_string();
                        let alias = cap.get(2).map(|m| m.as_str().trim().to_string());

                        let is_asset = is_asset_path(&target);
                        let kind = if is_asset { "asset" } else { "page" }.to_string();
                        let target_norm = normalize_target(&target, is_asset);

                        links.push(ExtractedLink {
                            kind,
                            is_embed: true,
                            target,
                            target_norm,
                            alias,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    let title = extract_title(path, frontmatter, body);

    PageMetadata {
        title,
        has_mermaid,
        tags,
        links,
        aliases,
    }
}

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum WatcherEvent {
    Modified(PathBuf),
    Deleted(PathBuf),
}

pub struct IndexerQueue {
    sender: mpsc::Sender<WatcherEvent>,
    _watcher: notify::RecommendedWatcher,
}

impl IndexerQueue {
    pub fn new(db_pool: PgPool, content_root: PathBuf) -> Result<Self> {
        let (sender, receiver) = mpsc::channel(1024);

        // 1. Run startup reconcile sweep in the background
        let db_clone = db_pool.clone();
        let content_root_clone = content_root.clone();
        tokio::spawn(async move {
            info!("Starting startup reconcile sweep...");
            if let Err(e) = Self::reconcile_all(&db_clone, &content_root_clone).await {
                error!("Startup reconcile sweep failed: {:?}", e);
            } else {
                info!("Startup reconcile sweep completed successfully.");
            }

            // Start consumer loop
            Self::run_consumer(receiver, db_clone, content_root_clone).await;
        });

        // 2. Set up notify watcher
        let sender_for_watcher = sender.clone();
        let content_root_for_watcher = content_root.clone();
        let mut watcher = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| match res {
                Ok(event) => {
                    for path in event.paths {
                        // Check if file is markdown
                        if path.extension().map_or(false, |ext| ext == "md") {
                            // Also exclude temporary files
                            if path.to_string_lossy().ends_with(".tmp") {
                                continue;
                            }
                            if let Ok(rel_path) = path.strip_prefix(&content_root_for_watcher) {
                                let w_event = if path.exists() {
                                    WatcherEvent::Modified(rel_path.to_path_buf())
                                } else {
                                    WatcherEvent::Deleted(rel_path.to_path_buf())
                                };
                                if let Err(e) = sender_for_watcher.try_send(w_event) {
                                    warn!("Failed to send watcher event to channel (might be full/closed): {:?}", e);
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Watcher error: {:?}", e);
                }
            },
            notify::Config::default(),
        )?;

        // Exclude .git and .trash if they exist in content_root (they shouldn't be under sedum/ but let's be safe)
        if content_root.exists() {
            watcher.watch(&content_root, notify::RecursiveMode::Recursive)?;
            info!(
                "Directory watcher successfully initialized on {:?}",
                content_root
            );
        } else {
            // Auto-create content root if not present
            fs::create_dir_all(&content_root)?;
            watcher.watch(&content_root, notify::RecursiveMode::Recursive)?;
            info!("Created and watched content root {:?}", content_root);
        }

        Ok(Self {
            sender,
            _watcher: watcher,
        })
    }

    pub fn trigger_reconcile(&self) {
        // If channel fills or other safety guards trigger
        let _ = self
            .sender
            .try_send(WatcherEvent::Modified(PathBuf::from("__reconcile__")));
    }

    async fn run_consumer(
        mut receiver: mpsc::Receiver<WatcherEvent>,
        db_pool: PgPool,
        content_root: PathBuf,
    ) {
        let mut debounce_buffer = HashSet::new();
        let debounce_duration = Duration::from_millis(300);

        while let Some(event) = receiver.recv().await {
            if event == WatcherEvent::Modified(PathBuf::from("__reconcile__")) {
                info!("Manual reconcile event triggered, running reconciliation...");
                if let Err(e) = Self::reconcile_all(&db_pool, &content_root).await {
                    error!("Reconciliation sweep failed: {:?}", e);
                }
                continue;
            }

            debounce_buffer.insert(event);

            // Drain any pending events immediately available
            while let Ok(evt) = receiver.try_recv() {
                if evt == WatcherEvent::Modified(PathBuf::from("__reconcile__")) {
                    debounce_buffer.clear();
                    let _ = Self::reconcile_all(&db_pool, &content_root).await;
                    break;
                }
                debounce_buffer.insert(evt);
            }

            if debounce_buffer.is_empty() {
                continue;
            }

            // Sleep to let more changes accumulate (debounce window)
            sleep(debounce_duration).await;

            // Drain again to capture anything that arrived during sleep
            while let Ok(evt) = receiver.try_recv() {
                if evt == WatcherEvent::Modified(PathBuf::from("__reconcile__")) {
                    debounce_buffer.clear();
                    let _ = Self::reconcile_all(&db_pool, &content_root).await;
                    break;
                }
                debounce_buffer.insert(evt);
            }

            // Process the accumulated batch
            if !debounce_buffer.is_empty() {
                let batch: Vec<WatcherEvent> = debounce_buffer.drain().collect();
                info!("Processing batch of {} filesystem events...", batch.len());
                if let Err(e) = Self::process_batch(batch, &db_pool, &content_root).await {
                    error!("Failed to index batch: {:?}", e);
                }
            }
        }
    }

    async fn process_batch(
        batch: Vec<WatcherEvent>,
        db_pool: &PgPool,
        content_root: &PathBuf,
    ) -> Result<()> {
        let mut to_upsert = Vec::new();
        let mut to_delete = Vec::new();

        for event in batch {
            match event {
                WatcherEvent::Modified(p) => to_upsert.push(p),
                WatcherEvent::Deleted(p) => to_delete.push(p),
            }
        }

        let mut tx = db_pool.begin().await?;

        // 1. Process deletions
        for path in to_delete {
            let path_str = path.to_string_lossy().to_string();
            info!("Indexing: removing page={}", path_str);
            sqlx::query("DELETE FROM tb_pages WHERE path = $1")
                .bind(&path_str)
                .execute(&mut *tx)
                .await?;
        }

        // 2. Process upserts
        for path in to_upsert {
            let file_path = content_root.join(&path);
            let path_str = path.to_string_lossy().to_string();

            if !file_path.exists() {
                // If it was modified but deleted before we read it
                sqlx::query("DELETE FROM tb_pages WHERE path = $1")
                    .bind(&path_str)
                    .execute(&mut *tx)
                    .await?;
                continue;
            }

            info!("Indexing: parsing/saving page={}", path_str);
            let raw_content = match fs::read_to_string(&file_path) {
                Ok(c) => c,
                Err(e) => {
                    error!("Failed to read file {:?}: {:?}", file_path, e);
                    continue;
                }
            };

            let mtime = match fs::metadata(&file_path) {
                Ok(meta) => meta.modified().map_or(0, |time| {
                    time.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0)
                }),
                Err(_) => 0,
            };

            let (frontmatter, body) = parse_markdown(&raw_content);
            let frontmatter_json =
                frontmatter.unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            let metadata = extract_metadata(&path_str, Some(&frontmatter_json), body);
            let slug = Path::new(&path_str)
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| path_str.clone());

            // Upsert tb_pages
            let row: (i64,) = sqlx::query_as(
                "INSERT INTO tb_pages (path, slug, title, frontmatter, has_mermaid, mtime, body_tsv) \
                 VALUES ($1, $2, $3, $4, $5, $6, setweight(to_tsvector('english', COALESCE($3, '')), 'A') || setweight(to_tsvector('english', COALESCE($7, '')), 'B')) \
                 ON CONFLICT (path) \
                 DO UPDATE SET slug = EXCLUDED.slug, title = EXCLUDED.title, frontmatter = EXCLUDED.frontmatter, \
                               has_mermaid = EXCLUDED.has_mermaid, mtime = EXCLUDED.mtime, body_tsv = EXCLUDED.body_tsv \
                 RETURNING id"
            )
            .bind(&path_str)
            .bind(&slug)
            .bind(&metadata.title)
            .bind(&frontmatter_json)
            .bind(metadata.has_mermaid)
            .bind(mtime)
            .bind(body)
            .fetch_one(&mut *tx)
            .await?;
            let page_id = row.0;

            // Clear old links, tags, aliases
            sqlx::query("DELETE FROM tb_links WHERE src_id = $1")
                .bind(page_id)
                .execute(&mut *tx)
                .await?;

            sqlx::query("DELETE FROM tb_tags WHERE page_id = $1")
                .bind(page_id)
                .execute(&mut *tx)
                .await?;

            sqlx::query("DELETE FROM tb_page_aliases WHERE page_id = $1")
                .bind(page_id)
                .execute(&mut *tx)
                .await?;

            // Insert tags
            for tag in metadata.tags {
                sqlx::query("INSERT INTO tb_tags (page_id, tag) VALUES ($1, $2) ON CONFLICT (page_id, tag) DO NOTHING")
                    .bind(page_id)
                    .bind(&tag)
                    .execute(&mut *tx)
                    .await?;
            }

            // Insert aliases
            for alias in metadata.aliases {
                sqlx::query("INSERT INTO tb_page_aliases (page_id, alias) VALUES ($1, $2) ON CONFLICT (page_id, alias) DO NOTHING")
                    .bind(page_id)
                    .bind(&alias)
                    .execute(&mut *tx)
                    .await?;
            }

            // Insert links (we'll resolve target_ids after all pages are saved/indexed)
            for link in metadata.links {
                let alias_opt = link.alias.as_deref();
                sqlx::query(
                    "INSERT INTO tb_links (src_id, kind, is_embed, target, target_norm, target_id, alias) \
                     VALUES ($1, $2, $3, $4, $5, NULL, $6) \
                     ON CONFLICT (src_id, kind, target_norm, is_embed) DO NOTHING"
                )
                .bind(page_id)
                .bind(&link.kind)
                .bind(link.is_embed)
                .bind(&link.target)
                .bind(&link.target_norm)
                .bind(alias_opt)
                .execute(&mut *tx)
                .await?;
            }

            // Resolve any dangling links pointing to this page
            sqlx::query(
                "UPDATE tb_links SET target_id = $1 WHERE kind = 'page' AND target_norm = $2 AND target_id IS NULL"
            )
            .bind(page_id)
            .bind(&slug)
            .execute(&mut *tx)
            .await?;
        }

        // 3. Resolve target_ids of all dangling links pointing to all pages
        // This is a bulk re-resolve query for any dangling links
        sqlx::query(
            "UPDATE tb_links l \
             SET target_id = p.id \
             FROM tb_pages p \
             WHERE l.kind = 'page' AND l.target_id IS NULL AND l.target_norm = p.slug",
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    async fn reconcile_all(db_pool: &PgPool, content_root: &Path) -> Result<()> {
        info!(
            "Walking content root {:?} to reconcile page database index...",
            content_root
        );
        let mut local_files = HashSet::new();

        // 1. Gather all local files
        if content_root.exists() {
            fn walk_dir(dir: &Path, files: &mut HashSet<PathBuf>) -> io::Result<()> {
                if dir.is_dir() {
                    for entry in fs::read_dir(dir)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.is_dir() {
                            walk_dir(&path, files)?;
                        } else if path.extension().map_or(false, |ext| ext == "md") {
                            // Exclude temp files
                            if !path.to_string_lossy().ends_with(".tmp") {
                                files.insert(path);
                            }
                        }
                    }
                }
                Ok(())
            }
            let _ = walk_dir(content_root, &mut local_files);
        }

        // 2. Query all database pages
        let db_pages: Vec<(String, i64)> = sqlx::query_as("SELECT path, mtime FROM tb_pages")
            .fetch_all(db_pool)
            .await?;

        let mut to_upsert = Vec::new();
        let mut to_delete = Vec::new();

        let mut db_paths_set = HashSet::new();
        for (path, mtime) in db_pages {
            let full_path = content_root.join(&path);
            db_paths_set.insert(path.clone());

            if !full_path.exists() {
                to_delete.push(WatcherEvent::Deleted(PathBuf::from(&path)));
            } else {
                let local_mtime = fs::metadata(&full_path).map_or(0, |meta| {
                    meta.modified().map_or(0, |time| {
                        time.duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0)
                    })
                });

                if local_mtime > mtime {
                    to_upsert.push(WatcherEvent::Modified(PathBuf::from(&path)));
                }
            }
        }

        // Check for new files not in database
        for file in local_files {
            if let Ok(rel_path) = file.strip_prefix(content_root) {
                let rel_path_str = rel_path.to_string_lossy().to_string();
                if !db_paths_set.contains(&rel_path_str) {
                    to_upsert.push(WatcherEvent::Modified(rel_path.to_path_buf()));
                }
            }
        }

        if !to_upsert.is_empty() || !to_delete.is_empty() {
            info!(
                "Reconcile details: {} updates, {} deletions",
                to_upsert.len(),
                to_delete.len()
            );
            let mut batch = to_upsert;
            batch.extend(to_delete);
            Self::process_batch(batch, db_pool, &content_root.to_path_buf()).await?;
        } else {
            info!("Database index is fully in sync with filesystem.");
        }

        Ok(())
    }
}
