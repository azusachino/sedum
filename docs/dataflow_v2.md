# Dataflow & Workflows (v2)

This document describes the updated dataflow and execution flows for the **RocksDB + Postgres + Filesystem Sync** architecture.

---

## 1. System Overview (v2)

The web server reads and writes primary content from the local **RocksDB** store. **Postgres** acts as the relational index cache for analytical queries (links, tags, search vectors), and the **Local Filesystem** acts as an asynchronous persistence target.

```mermaid
flowchart TD
    Browser["Browser / Client"]

    subgraph Server["sedum — Rust single binary"]
        HTTP["axum HTTP Layer"]
        Rocks["RocksDB\n(Primary Store: Raw Markdown)"]
        IndexerTask["Postgres Indexer Task\n(Async Loop)"]
        FileSyncTask["Filesystem Sync Task\n(Async Loop)"]
    end

    PG[("Postgres\n(Index Cache:\nbody_tsv, links, tags)")]
    FS[("sedum/ Markdown files\n(Local Sync Target)")]

    Browser -->|"GET view / edit"| HTTP
    Browser -->|"POST save"| HTTP
    HTTP -->|"Read/write body"| Rocks

    HTTP -->|"FTS / Tag queries"| PG
    HTTP -->|"Fetch matching bodies\n& build snippets"| Rocks

    Rocks -.->|"Read dirty keys"| IndexerTask
    IndexerTask -->|"Write index records"| PG

    Rocks -.->|"Read dirty keys"| FileSyncTask
    FileSyncTask -->|"Write .md files"| FS
```

---

## 2. Rendering Model — View vs. Edit

The rendered view reads directly from RocksDB. Pages are edited via a `<textarea>` and saved instantly back to RocksDB.

```mermaid
flowchart TD
    V["GET /page/Foo"] --> X{"Foo exists in RocksDB?"}
    X -- yes --> R["Read from RocksDB → render HTML"] --> RO["Readonly view"]
    X -- no --> NEW["Offer: create page Foo?"]

    RO -->|"Click Edit"| E["GET /page/Foo/edit"]
    E --> T["Read from RocksDB → textarea"]
    T -->|"Save"| P["POST /page/Foo"]
    P --> S["Write page content to RocksDB\nSet sync_needed = true\nSet index_needed = true"]
    S --> RD["303 redirect → /page/Foo (view)"]
    RD --> V
```

---

## 3. Save $\rightarrow$ Sync & Index Pipeline

Saves write to RocksDB and return immediately. Background workers perform database indexing and filesystem writes asynchronously.

```mermaid
sequenceDiagram
    participant B as Browser
    participant H as axum handler
    participant R as RocksDB (KV)
    participant I as Indexer Task
    participant PG as Postgres
    participant S as File Sync Task
    participant FS as sedum/*.md

    B->>H: POST /page/Foo (markdown body)
    H->>R: Write key page:foo (body, sync_needed=true, index_needed=true)
    H-->>B: 303 Redirect to page view
    Note over H,R: Handler returns in <5ms

    par Postgres Indexing (Every 300ms)
        I->>R: Scan for keys where index_needed = true
        R-->>I: Return changed pages
        I->>I: Parse comrak links, tags, FTS vector
        I->>PG: Upsert index rows (Single Transaction)
        I->>R: Update index_needed = false
    and Filesystem Persistence (Every 1-2s)
        S->>R: Scan for keys where sync_needed = true
        R-->>S: Return changed pages
        S->>FS: Write sub/Bar.md (Write temp + rename)
        S->>R: Update sync_needed = false
    end
```

---

## 4. FTS Search & Snippet Generation Flow

Search queries run on Postgres's GIN vector index. Search snippets are generated in Rust memory after loading matching page bodies from RocksDB.

```mermaid
sequenceDiagram
    participant B as Browser
    participant H as axum handler
    participant PG as Postgres (Index)
    participant R as RocksDB (Store)

    B->>H: GET /search?q=postgres
    H->>PG: SELECT slug, title FROM tb_pages WHERE body_tsv @@ to_tsquery('postgres')
    PG-->>H: Return matching metadata (e.g. 10 page slugs)
    loop For each matching slug
        H->>R: Point read raw body text
        R-->>H: Return body Markdown
        H->>H: Extract regex snippet and wrap matches in <mark>
    end
    H-->>B: Render search results page with highlighted snippets
```

---

## 5. Directory Reconciliation (Manual Import/Sync)

To handle external filesystem changes (e.g., git pulls, external editor writes), a reconciliation process is triggered manually or by CLI.

```mermaid
flowchart TD
    A["Trigger Sync"] --> B["Walk sedum/**/*.md on disk"]
    B --> C{"file.mtime > RocksDB.mtime?"}
    C -- Yes --> D["Read file content\nWrite to RocksDB\nSet index_needed=true\nSet sync_needed=false"]
    C -- No --> E["Skip"]
    
    A --> F["Find keys in RocksDB absent on disk"]
    F --> G["Delete page from RocksDB\nTrigger Postgres cascade delete"]
    
    D --> H["Trigger Postgres Indexer sweep"]
    G --> H
```
