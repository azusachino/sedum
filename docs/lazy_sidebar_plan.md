# Architecture Proposal: Lazy Loading Sidebar Navigation Tree

This plan details the migration of Miku Wiki's sidebar file explorer from a full-tree static render (O(N) scalability bottleneck) to an on-demand, lazy-loaded folder tree (O(1) footprint).

---

## 1. The Scaling Problem

Currently, Miku Wiki queries and renders the entire nested folder tree of the vault into HTML on every page request:
* **Current Payload (2k notes)**: Serves **3.96 MB of HTML** on every request. This takes **146ms** for the server to process, and **~2,000ms** for the browser to parse/paint.
* **100k Note Projection**: Serves **~200 MB of HTML** on every request. The server will take seconds to render it, and the browser's tab will run out of memory and crash.

To support 100,000+ notes, we must shift to a **lazy-loaded folder tree** where the server only renders root-level items initially, and folders are expanded via background AJAX swaps on click.

---

## 2. Step 1: Design the Backend API

We will add a new endpoint `/api/nav/children` to retrieve and render the immediate contents of any folder path.

### 1. Request Signature
* **Route**: `GET /api/nav/children?dir={folder_path}`
* **Parameters**: 
  - `dir`: Optional directory path slug (empty string represents the root of the vault).

### 2. Database Queries
Instead of loading all pages, the server queries *only* the direct descendants of the requested directory:
* **Root level query** (where `dir` is empty):
  - Retrieve all page paths containing no `/` character (root files).
  - Retrieve the first path segment of all paths containing a `/` (unique root folders).
* **Nested level query** (where `dir` is `Notes/Daily`):
  - Retrieve all page paths matching `Notes/Daily/*` but containing no further slashes (direct child files of `Notes/Daily`).
  - Retrieve the next segment of all paths starting with `Notes/Daily/` (direct child subfolders of `Notes/Daily`).

### 3. Response
* The endpoint renders a partial HTML list of nodes (either files or subfolders) using a MiniJinja fragment template, returning it with HTTP 200.

---

## 3. Step 2: Update Templates for Lazy Loading

We will modify the templates to bridge Alpine.js states with HTMX AJAX trigger requests.

### 1. Folder Node Markup in `base.html`
```html
<div class="tree-folder" x-data="{ open: false, loaded: false }">
    <div class="tree-folder-row flex items-center cursor-pointer select-none"
         @click="open = !open; if (!loaded) { loaded = true; htmx.trigger($refs.loader, 'load'); }">
        <span class="chevron text-[9px] mr-1" x-text="open ? '▼' : '▶'"></span>
        <span class="folder-name">{{ node.name }}</span>
    </div>
    
    <div class="tree-children pl-4" x-show="open" x-ref="loader"
         hx-get="/api/nav/children?dir={{ folder_path }}"
         hx-trigger="load"
         hx-swap="innerHTML"
         x-cloak>
        <span class="text-xs text-muted/40 italic p-1 block">Loading...</span>
    </div>
</div>
```

### 2. Root rendering
* On initial load of the sidebar, the page template only retrieves the root-level list of files and folders.
* The initial page HTML payload drops from **4 MB** to **< 15 KB**.

---

## 4. Step 3: Handle Active Note Path Pre-expansion

When the user is viewing a note deep inside a folder structure (e.g. `/p/note/books/2026/think-again`):
* We must ensure the folder tree is pre-expanded to reveal this note, otherwise they won't see it in the sidebar.
* **Mechanism**:
  - The server builds a set of "active ancestor paths" for the open note (e.g. `note`, `note/books`, `note/books/2026`).
  - In the template, if a folder path is in the ancestors set, we pre-initialize Alpine's state to `{ open: true, loaded: true }` and render its children immediately from the server.
  - All other sibling/unrelated folders remain unrendered (`loaded: false`), keeping the HTML payload tiny.

---

## 5. Verification Strategy

1. **Unit Tests**: Add tests in `src/main.rs` verifying the `/api/nav/children` endpoint returns the correct files and subfolder nodes for a mocked directory structure.
2. **E2E Scaling Benchmark**: Run the Python script `e2e_perf_test.py` to compare page size and latency after the lazy-loading implementation, verifying it handles larger counts (like 500+ files) without any latency increase.
