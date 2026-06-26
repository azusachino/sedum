//! Shared Markdown helpers used by both the HTTP renderer (`main.rs`) and the
//! background indexer (`indexer.rs`): frontmatter parsing, title extraction,
//! wikilink normalization, and HTML rendering with resolved wikilinks/embeds.
//!
//! Keeping parse and render on the *same* comrak options is load-bearing: the
//! indexer records which `[[links]]`/`![[embeds]]` exist, and the renderer must
//! tokenize them identically or the index and the page would disagree.

use comrak::nodes::{AstNode, NodeValue};
use regex::Regex;
use std::path::Path;

lazy_static::lazy_static! {
    /// Matches `![[target]]` and `![[target|label]]` embeds. Comrak does *not*
    /// recognize the `![[...]]` form (it leaves the whole run as a text node),
    /// so embeds are detected by regex over text nodes in both the indexer and
    /// the renderer — never as `WikiLink` AST nodes.
    pub static ref EMBED_REGEX: Regex =
        Regex::new(r"!\[\[([^\]|]+)(?:\|([^\]]+))?\]\]").unwrap();
}

/// Split YAML frontmatter from the Markdown body. Returns the parsed
/// frontmatter (as JSON) and the body slice after the closing `---`.
pub fn parse_frontmatter(content: &str) -> (Option<serde_json::Value>, &str) {
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

/// Title resolution: frontmatter `title`, else the first `# H1`, else the
/// path basename.
pub fn extract_title(path: &str, frontmatter: Option<&serde_json::Value>, body: &str) -> String {
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

/// True when a link target names a binary asset (embedded with `![[file.png]]`)
/// rather than another page.
pub fn is_asset_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".svg")
        || lower.ends_with(".pdf")
        || lower.ends_with(".webp")
}

/// Normalize a wikilink target into the resolver key stored in `tb_pages.slug`
/// / `tb_links.target_norm`: lowercased, with a trailing `.md` stripped for
/// pages (assets keep their extension).
pub fn normalize_target(name: &str, is_asset: bool) -> String {
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

/// Comrak options shared by parse (indexer) and render (HTTP). Render-only
/// flags (`render.r#unsafe`) are layered on top by [`render_html`].
pub fn comrak_options() -> comrak::Options<'static> {
    let mut options = comrak::Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    options.extension.alerts = true;
    options.extension.wikilinks_title_after_pipe = true;
    options
}

/// Concatenate the visible text of a wikilink node's children (its display
/// label / alias).
fn node_label<'a>(node: &'a AstNode<'a>) -> String {
    let mut label = String::new();
    for child in node.children() {
        for desc in child.descendants() {
            if let NodeValue::Text(t) = &desc.data.borrow().value {
                label.push_str(t);
            }
        }
    }
    label
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Replace a node with a single raw-HTML inline, dropping its children so only
/// the injected markup renders.
fn set_html_inline<'a>(node: &'a AstNode<'a>, html: String) {
    let children: Vec<_> = node.children().collect();
    for c in children {
        c.detach();
    }
    node.data.borrow_mut().value = NodeValue::HtmlInline(html);
}

/// Render `[[Target]]` / `[[Target|label]]` as an anchor to the `/p/` page
/// route, tagged `wikilink-missing` when `resolved` reports the slug is absent.
fn anchor_html(target: &str, label: &str, resolved: &dyn Fn(&str) -> bool) -> String {
    let label = if label.is_empty() { target } else { label };
    let norm = normalize_target(target, false);
    let class = if resolved(&norm) {
        "wikilink"
    } else {
        "wikilink wikilink-missing"
    };
    format!(
        "<a href=\"{}\" class=\"{}\">{}</a>",
        escape_attr(&format!("/p/{}", target.trim())),
        class,
        escape_text(label)
    )
}

/// Rewrite a text node that contains `![[...]]` embeds into HTML: asset embeds
/// become `<img>` under `/assets/`; page embeds fall back to a page link (no
/// transclusion in the MVP). Surrounding text is HTML-escaped, since the result
/// is injected verbatim as raw HTML.
fn render_text_with_embeds(text: &str, resolved: &dyn Fn(&str) -> bool) -> String {
    let mut out = String::new();
    let mut last = 0;
    for cap in EMBED_REGEX.captures_iter(text) {
        let m = cap.get(0).unwrap();
        out.push_str(&escape_text(&text[last..m.start()]));

        let target = cap.get(1).map_or("", |x| x.as_str()).trim();
        let label = cap
            .get(2)
            .map(|x| x.as_str().trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(target);

        let html = if is_asset_path(target) {
            let src = format!("/assets/{}", normalize_target(target, true));
            format!(
                "<img src=\"{}\" alt=\"{}\" class=\"wiki-embed\">",
                escape_attr(&src),
                escape_attr(label)
            )
        } else {
            anchor_html(target, label, resolved)
        };
        out.push_str(&html);
        last = m.end();
    }
    out.push_str(&escape_text(&text[last..]));
    out
}

/// Render a Markdown body to HTML, rewriting `[[wikilinks]]` to `/p/` page
/// routes (with a `wikilink-missing` class when `resolved` reports the target
/// slug is absent) and `![[file.png]]` asset embeds to `<img>` under
/// `/assets/`.
///
/// `resolved` is called with a normalized page slug (see [`normalize_target`]).
/// Raw HTML passthrough is enabled — this is a single-user, local wiki over
/// trusted files, so users may hand-write HTML in their notes.
pub fn render_html(body: &str, resolved: &dyn Fn(&str) -> bool) -> String {
    let arena = comrak::Arena::new();
    let mut options = comrak_options();
    options.render.r#unsafe = true;
    let root = comrak::parse_document(&arena, body, &options);

    // Collect first, then mutate: detaching children mid-traversal would
    // corrupt the descendants() iterator. `[[ ]]` links are WikiLink nodes;
    // `![[ ]]` embeds are never tokenized by comrak and surface as text.
    let nodes: Vec<&AstNode> = root.descendants().collect();
    for node in nodes {
        enum Rewrite {
            Link(String),
            EmbedText(String),
        }
        let rewrite = match &node.data.borrow().value {
            NodeValue::WikiLink(w) => Some(Rewrite::Link(w.url.clone())),
            NodeValue::Text(t) if EMBED_REGEX.is_match(t) => {
                Some(Rewrite::EmbedText(t.to_string()))
            }
            _ => None,
        };

        match rewrite {
            Some(Rewrite::Link(url)) => {
                let html = anchor_html(&url, &node_label(node), resolved);
                set_html_inline(node, html);
            }
            Some(Rewrite::EmbedText(text)) => {
                set_html_inline(node, render_text_with_embeds(&text, resolved));
            }
            None => {}
        }
    }

    // Writing to a String is infallible, so the formatter result is ignored.
    let mut out = String::new();
    let _ = comrak::format_html(root, &options, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter() {
        let content = "---\ntitle: Hello World\n---\n# Header\nBody content";
        let (yaml, body) = parse_frontmatter(content);
        assert!(yaml.is_some());
        assert_eq!(
            yaml.unwrap().get("title").and_then(|t| t.as_str()),
            Some("Hello World")
        );
        assert_eq!(body, "# Header\nBody content");
    }

    #[test]
    fn test_parse_frontmatter_none() {
        let content = "# Header\nBody content";
        let (yaml, body) = parse_frontmatter(content);
        assert!(yaml.is_none());
        assert_eq!(body, "# Header\nBody content");
    }

    #[test]
    fn test_render_page_link_resolved_vs_missing() {
        let html = render_html("See [[Foo]] and [[Bar]].", &|norm| norm == "foo");
        assert!(html.contains(r#"<a href="/p/Foo" class="wikilink">Foo</a>"#));
        assert!(html.contains(r#"<a href="/p/Bar" class="wikilink wikilink-missing">Bar</a>"#));
    }

    #[test]
    fn test_render_aliased_link() {
        let html = render_html("[[Target|Display]]", &|_| true);
        assert!(html.contains(r#"<a href="/p/Target" class="wikilink">Display</a>"#));
    }

    #[test]
    fn test_render_asset_embed() {
        let html = render_html("![[diagram.png]]", &|_| false);
        assert!(html.contains(r#"<img src="/assets/diagram.png""#));
        // The leading `!` must not leak into the output.
        assert!(!html.contains(">!<"));
        assert!(!html.contains("!<img"));
    }

    #[test]
    fn test_render_page_embed_falls_back_to_link() {
        let html = render_html("![[Page]]", &|_| true);
        assert!(html.contains(r#"<a href="/p/Page" class="wikilink">Page</a>"#));
        assert!(!html.contains("!<a"));
    }
}
