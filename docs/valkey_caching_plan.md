# Architecture Proposal: Valkey Caching Layer

This plan details the implementation of an out-of-process caching layer using **Valkey 9.1** to speed up page renders, save CPU compilation cycles, and handle 100,000+ notes under high request volumes.

---

## 1. The Bottleneck

1. **PostgreSQL Overhead**: Every note read requires multiple queries to fetch the note record, backlinks (`tb_links`), and page indexing details.
2. **Markdown Compilation Cost**: Parsing markdown with `comrak`, rendering HTML headers, resolving wikilinks, parsing admonitions, and formatting tables is a heavy CPU-bound task. For notes with complex markdown, this adds 5-25ms of latency per render.
3. **Container Restarts**: In-memory caching is lost when the app container is rebuilt or restarted (cold-cache penalty). An out-of-process cache in Valkey persists cache records across development runs and deployments.

---

## 2. Step 1: Add Valkey Container

We will add the official **Valkey 9-alpine** image to Miku's Docker compose configuration:

### Changes in `compose.yml`:
```yaml
services:
  valkey:
    image: docker.io/valkey/valkey:9-alpine
    container_name: miku-valkey
    healthcheck:
      test: ["CMD", "valkey-cli", "ping"]
      interval: 3s
      timeout: 3s
      retries: 10

  miku:
    ...
    depends_on:
      postgres:
        condition: service_healthy
      valkey:
        condition: service_healthy
    environment:
      DATABASE_URL: postgres://miku:miku@postgres:5432/miku
      VALKEY_URL: redis://valkey:6379/
```

---

## 3. Step 2: Rust Client Integration

We will use the standard **`redis`** crate (fully compatible with Valkey's wire protocol) to connect to Valkey.

### Changes in `Cargo.toml`:
```toml
[dependencies]
redis = { version = "0.25", features = ["tokio-comp"] }
```

### Changes in `AppState`:
Inject the Valkey connection pool or client into `AppState`:
```rust
struct AppState {
    db: sqlx::PgPool,
    templates: Arc<Environment<'static>>,
    events: tokio::sync::broadcast::Sender<String>,
    valkey: redis::Client,
}
```

---

## 4. Step 3: Caching Logic in `page_view`

We will store pre-rendered HTML page layouts in Valkey, caching the rendered markdown body fragments.

### Key Scheme:
We key cached note html by its file path and its document hash:
```
miku:page:{path_slug}:{body_hash}
```

### Flow in `page_view`:
1. Calculate/fetch the file's `body_hash` from the filesystem or database.
2. Query Valkey: `GET miku:page:{path}:{body_hash}`.
3. **If Hit**: Return the cached HTML string immediately. (Estimated latency: **< 1ms**).
4. **If Miss**:
   - Fetch backlinks and page details from PostgreSQL.
   - Run the Comrak parser to render Markdown to HTML.
   - Save the rendered HTML to Valkey with an expiration window (e.g. 7 days).
   - Return the rendered HTML.

---

## 5. Invalidation Strategy

* **Self-Invalidating Keys**: Since the `body_hash` is part of the cache key, any modification to a note updates its hash, rendering the old key dead immediately. This avoids cache-coherency bugs.
* **Hard Invalidation (Renames/Deletions)**: When a note is deleted or renamed, we issue a `DEL` command to Valkey for the old path's cache keys to prevent orphans.

---

## 6. Verification Strategy

1. **Integration Verification**: Check container logs to verify `miku-valkey` starts up healthy and `miku-app` successfully connects to the client.
2. **Micro-benchmarks**: Compare the HTTP request latencies of a note view on cache hit vs. cache miss, expecting a **> 90% latency reduction** on cache hits.
