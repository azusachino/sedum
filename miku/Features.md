# Miku Features

Miku combines filesystem simplicity with wiki intelligence. This page describes the core features that make the system work. #feature #guide

## Wikilinks

Pages are connected using the `[[PageName]]` syntax. When you write `[[Index]]`, Miku automatically renders it as a clickable link to the Index page. Wikilinks are the foundation of knowledge organization in Miku — they let you think in a network of ideas rather than a linear hierarchy.

The wikilink parser respects case-insensitive page matching, so `[[index]]` and `[[Index]]` both work. Behind the scenes, the indexer crawls your Markdown files, extracts every wikilink, and builds a graph.

## Backlinks

Write a [[Features]] link on any page, and Miku automatically shows you everywhere [[Features]] is mentioned. Backlinks surface unexpected connections and help you navigate your wiki without manually maintaining "see also" lists.

The backlinks panel loads on demand, so large, densely-connected wikis stay responsive. See the architecture docs for pagination strategy.

## Tags

Sprinkle `#hashtags` naturally in your prose. Miku extracts them, indexes them, and provides a tag-based filter view. Unlike rigid category systems, tags are informal and additive — the same page can have #docs, #feature, and #guide simultaneously.

The tag index is rebuilt as you save, and the `/tags` view lets you browse pages grouped by tag or navigate to a single tag's pages. #feature

## Full-Text Search

Every page is indexed for full-text search. Type in the search box and find any phrase or word across your entire wiki. The search respects Markdown structure, so searches for content in code blocks and links work as expected. #feature

Search is powered by Postgres full-text indexing and runs asynchronously, so it never blocks edits. Results are ranked by relevance.

## Atomic Saves

When you save a page, Miku writes to a temporary file, then atomically renames it into place. This guarantees that the wiki is never left in a partially-written state — even if the server crashes mid-save, your data stays consistent.

The atomic save also triggers the background indexer to refresh affected pages only, so you never wait for a full re-index. #feature

## Background Indexer

The indexer runs continuously in the background, watching the `miku/` directory for changes. It is the sole writer to the Postgres index — HTTP handlers only read. This single-writer model eliminates races and double-indexing bugs.

When a page is edited or created, the indexer extracts wikilinks, tags, and full-text content, updating the pages, links, and tags tables in Postgres. The index is fully rebuildable from your `.md` files, so it's safe to drop and recreate at any time. See [[Usage]] for how to trigger a full re-index. #feature

## No Fragmentation

Because your wiki lives in `miku/` alongside your git history, every page is version-controlled by default. You can see when [[Index]] was last edited, revert a broken save, and audit changes over time. Miku never creates orphaned or unreachable pages — every file is either on disk or deleted and gone from history.

---

For a hands-on introduction, see [[Sandbox]]. To get Miku running, see [[Usage]].
