# Architecture

Sedum is a filesystem-owned personal Markdown wiki: a browser editor over plain
`.md` files, with server-side background indexing for backlinks, tags, and
full-text search.

## Core invariant

Markdown files and assets under `sedum/` are the **source of truth**. Postgres
holds only a **disposable index** that is fully rebuildable from
`sedum/**/*.md`. Deleting the database loses nothing but rebuild time.

```
repo/
  sedum/            # content root — the vault (source of truth)
    *.md            # pages
    assets/         # images (roadmap: drag/drop upload)
  src/              # Rust server
  migrations/       # sqlx Postgres migrations (index schema)
  static/           # server-rendered frontend assets
```

## Components

- **HTTP layer (axum):** page render/edit/save routes, search, tags, backlinks,
  static asset serving. **Read-only** against Postgres.
- **Store:** filesystem read/write of `sedum/*.md`. Atomic save = write temp +
  `fsync` + `rename`.
- **Background indexer:** `notify` watcher on `sedum/`; parses changed pages off
  the request path into Postgres. The **sole writer**.
- **Postgres index:** `pages`, `links`, `tags`, and a `tsvector` FTS column.

## Save / index contract (single-writer model)

This is the key design decision that removes save↔index races:

1. `POST /save` writes to a temp file → `fsync` → atomic `rename` into
   `sedum/<path>.md`. The handler returns immediately and **does not touch the
   index**.
2. The `rename` fires a `notify` event → debounced (~200ms) → the indexer
   reindexes just that page.
3. **`notify` is the only index trigger.** Handlers never index directly, so
   there is no double-indexing and no race. Cost: backlinks lag a save by
   ~200ms — invisible for a personal wiki.
4. **Startup reconcile:** full scan comparing file mtime vs `pages.mtime` to
   catch anything `notify` missed while the process was down.

Reindex-one-page is a single transaction: upsert the `pages` row, wipe+rewrite
that page's `links` / `tags` / FTS rows, then re-resolve any dangling links now
pointing at this page.

Postgres runs with the standard single-writer pattern here: only the indexer
writes, HTTP handlers only read.

## Database schema (Postgres)

```sql
CREATE TABLE pages (
  id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  path       TEXT NOT NULL UNIQUE,   -- relative to sedum/, e.g. 'sub/Bar.md'
  title      TEXT NOT NULL,          -- first H1, else filename sans .md
  mtime      BIGINT NOT NULL,        -- file mtime (unix), for reconcile
  body_tsv   TSVECTOR,               -- full-text search vector
  indexed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_pages_tsv ON pages USING GIN (body_tsv);

-- directed link edges; one row per (page, target). target_id NULL = dangling
CREATE TABLE links (
  src_id    BIGINT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  target    TEXT   NOT NULL,         -- link name as written
  target_id BIGINT REFERENCES pages(id) ON DELETE SET NULL,
  alias     TEXT,
  PRIMARY KEY (src_id, target)
);
CREATE INDEX idx_links_target_id ON links(target_id);  -- backlinks
CREATE INDEX idx_links_target    ON links(target);     -- dangling resolve

CREATE TABLE tags (
  page_id BIGINT NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  tag     TEXT   NOT NULL,
  PRIMARY KEY (page_id, tag)
);
CREATE INDEX idx_tags_tag ON tags(tag);
```

Optional: enable `pg_trgm` for fuzzy title/link matching.

### Key queries

Backlinks (paginated — never loads the full edge set):

```sql
SELECT p.path, p.title
FROM links l JOIN pages p ON p.id = l.src_id
WHERE l.target_id = $1
ORDER BY p.title LIMIT $2 OFFSET $3;
```

Dangling → resolved: when a page is created that something already `[[linked]]`
to, the indexer runs
`UPDATE links SET target_id = $1 WHERE target = $2 AND target_id IS NULL`, and
the backlink appears. `ON DELETE SET NULL` turns inbound links dangling again
when a page is deleted.

## Rendering model (view / edit)

The readonly rendered view is the **primary** mode; editing is opt-in (classic
wiki model, no client JS):

- `GET /page/Foo` → render `Foo.md` → HTML (readonly view, default)
- `GET /page/Foo/edit` → `<textarea>` with raw Markdown
- `POST /page/Foo` → atomic save → 303 redirect back to the readonly view

A `SEDUM_READONLY` flag (roadmap) gates the edit/save routes for publishing.
Rich rendering beyond CommonMark is done with **server-side directives**
(`:::note` callouts, `![[transclusion]]`) parsed over pulldown-cmark — never
MDX/JSX (see `docs/decisions` / asobi `sedum:decision:no-mdx`).

See `docs/dataflow.md` for the full set of workflow / dataflow Mermaid diagrams.

## Link resolution

`[[Name]]` resolves by unique basename across `sedum/`; if multiple match, pick
deterministically (shortest path). `[[sub/Bar]]` matches an exact relative
path. Obsidian `[[ ]]` compatible — this replaces a separate importer.

## Out of scope

- **Roadmap:** drag/drop image upload to `assets/`, CodeMirror 6 editor,
  side-by-side live preview (pairs with CM6), `SEDUM_READONLY` publish flag,
  server-side rich-rendering directives (`:::note`, transclusion)
- **Rejected:** MDX/JSX — needs a JS runtime, breaks plain-Markdown portability
- **Dropped:** daily note, Obsidian vault import
- **Postponed (per design decision):** CRDT, canvas, Notion-style databases,
  plugin runtime, mobile offline sync
