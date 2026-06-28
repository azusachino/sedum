# Frontend Rework Plan — Performance Verification + UX Overhaul

Status: **Proposal (awaiting approval)** · Date: 2026-06-28 · Branch: `feat/mvp`

Motivation: the current frontend UX is poor despite a fast backend. This doc
(1) verifies current performance on both sides, (2) audits what ships to the
browser, (3) surveys popular OSS markdown-wiki frontends, and (4) proposes a
phased rework. **No implementation has been done** beyond measurement — the
plan below is for review.

---

## 1. Current-state verification

### 1.0 CRITICAL — the indexer does not scale (headline finding)

Re-benchmarked against a real corpus: `git clone --depth=1` of
`uaxe/geektime-docs` = **10,248 markdown files** into the vault.

- A long-running container masked everything: its bind mount was baked to the
  **pre-rename path** `.../sedum/miku`; after the host dir rename it silently
  served a stale/empty vault and couldn't restart. The earlier 5-page benchmark
  ran against this. Fix: full `podman compose down && up` to regenerate mounts.
- The running `notify` watcher **did not pick up the bulk clone** (inotify not
  propagating through the rootless podman bind mount); only a restart triggers a
  full rescan.
- On rescan the indexer entered a **re-index storm / livelock**: **84,101
  index operations for only 8,879 unique pages (~9.5× each)**, 3 concurrent
  ACTIVE Postgres connections (contradicts the documented single-writer model),
  and **`tb_pages` stayed 0 the entire ~5 minutes** — the full scan appears to
  run as one uncommitted mega-transaction, so the site shows zero pages during
  the whole rescan and never converged.

**Implication: page-serving performance is moot at scale because indexing never
completes.** This outranks the frontend CDN issue. Root causes to fix: no event
debounce/coalescing, no incremental commit, no unchanged-file skip (mtime/hash),
no single-writer serialization. Ties to `docs/watcher_and_queue_plan.md` + ADR-0006.

### 1.1 Backend page-serving (measured, tiny vault only)

Method: `oha -n 2000 -c 50` and `curl -w` against the running podman stack
(`miku-app` :3000, postgres healthy). **Caveats:** container is a *debug*-profile
build (Dockerfile drops `--release`), the vault is the tiny seeded 5-page demo,
and the image had ~34h uptime (predates the latest htmx/CM6 commits, which are
client-side only). Numbers are a floor for backend latency, not a scale test.

| Endpoint        | req/s | avg    | fastest | slowest | HTML size |
| --------------- | ----- | ------ | ------- | ------- | --------- |
| `/p/Index`      | 1836  | 26.9ms | 19.4ms  | 78.4ms  | 49.5 KB   |
| `/search?q=…`   | 2457  | 20.2ms | 5.4ms   | 29.0ms  | 49.2 KB   |
| `/tags`         | 5717  | 8.6ms  | 1.0ms   | 16.4ms  | 46.7 KB   |
| `/static/miku.css` | —  | —      | —       | —       | 33.6 KB   |
| `/static/miku.js`  | —  | —      | —       | —       | 7.6 KB    |

Single-request TTFB: ~5ms for pages, ~1ms for static. **Verdict: the backend is
not the bottleneck at this size.** Two structural concerns remain:

- **Every page render embeds the entire nav tree** (O(n) over all pages). Per
  `docs/lazy_sidebar_plan.md`, at 2k notes this is **3.96 MB HTML / 146ms server
  / ~2s browser paint** per request; at 100k it OOMs the tab. Not yet fixed.
- **~50 KB of HTML is re-sent on every navigation** — the full 714-line shell
  plus inline scripts ride along with each page.

### 1.2 Frontend audit (the likely UX culprit)

`src/templates/base.html` (714+ lines) loads **six render-blocking third-party
resources at runtime**:

| Resource | Source | Problem |
| -------- | ------ | ------- |
| `@tailwindcss/browser@4` | unpkg | **Tailwind Play CDN — compiles CSS in the browser at runtime** (~112 KB JS, JIT on every load). Tailwind docs: *"should not be used in production."* No tree-shaking, FOUC, CPU burn. |
| `alpinejs@3.x.x` | unpkg | remote, version-floating (`3.x.x`), render-block |
| `htmx.org@1.9.10` | unpkg | remote |
| `mermaid@10` | jsdelivr | large; loaded even when not needed (should be index-flagged per ADR-0007) |
| `prism-core` + `autoloader` | cdnjs | remote |
| `prism-tomorrow.css` | cdnjs | remote theme |

**This violates the project's own ADR-0007**, which mandates *"vendored,
locally-served (`/static/js/vendor/`), offline-capable"* scripts. The current
setup is offline-broken, privacy-leaky (six third-party origins), version-drifty
(`3.x.x`, `@4`), and render-blocking on first paint.

### 1.3 Test / verification coverage (gap)

- **36 unit tests** (template rendering, nav-tree, markdown, path-safety) — all green.
- **Zero HTTP/e2e integration tests.** No test boots axum + Postgres and exercises
  real routes (page save, search, tags, `/api/move`, `/api/trash`, SSE `/events`).
- The `e2e_perf_test.py` referenced by `docs/lazy_sidebar_plan.md` **does not exist.**
- No `make bench` / payload-regression guard.

This is exactly the recorded pitfall *"`make check` green ≠ working"* — the
riskiest paths (SSE live-sync, create-modal, save) are unverified by automation,
and four tasks sit `AWAITING_VERIFY` (redesign 4/7/8, fs-sidebar 5).

---

## 2. OSS landscape (UX / styling references)

| Project | Stack / UX | What to borrow |
| ------- | ---------- | -------------- |
| **SilverBullet** | Rust backend + **CodeMirror 6** + Preact, ESBuild bundle; live-preview editor, Page Picker (⌘-style), bidirectional links | Closest analog (also Rust+markdown+CM6). Validates: CM6 live-preview, command-palette navigation, thin pre-built client served by Rust. |
| **Quartz** | Static digital-garden publisher; refined typographic reading view, graph view, TOC | Reading-view typography, prose spacing, backlink/TOC presentation. |
| **TriliumNext** | Power-user notes; modernizing a dated UI; title-edit UX | Already borrowed (title field). Pattern source for properties/metadata panels. |
| **Logseq** | Electron, block/outliner UX | Less relevant (we're document-first, not block-first). |

Key external fix (verified via Tailwind docs): **Tailwind v4 ships a standalone
CLI** — a single binary, *no npm/Node* — that scans templates and emits a tiny,
cacheable static CSS file (`@import "tailwindcss";`, no config). This is the
precise replacement for the browser CDN and fits the "no bundler" constraint
(it's a devShell binary + a `make` target, not a JS build pipeline).

---

## 3. Proposed plan (phased)

Ordering principle: **make change safe before making it big.** Lock behavior with
tests, then fix the asset pipeline (biggest UX win, lowest risk), then decompose,
then polish UX.

### Phase 0a — Indexer scalability (NEW, now top priority)
Root-caused: a single NUL-byte file aborts the whole-vault transaction → 0 pages
committed → a 5s periodic reconcile retries the same failing batch forever.
Full diagnosis + fix list in **`docs/indexer_scalability_plan.md`**. Critical trio:
sanitize NUL/invalid bytes, per-file transaction isolation, in-flight reconcile
guard. Then incremental commit, relax 5s interval, bound SSE fan-out, quarantine
bad files. See also `docs/watcher_and_queue_plan.md`, ADR-0006,
`miku:pitfall:indexer-reindex-storm`.

### Phase 0 — Verification foundation
- Add HTTP **e2e integration tests** (axum router + ephemeral Postgres) covering:
  page render, save+reindex, search, tags, `/api/move`, `/api/trash`, `/events` SSE.
- Add the missing scale/payload benchmark (`scripts/e2e_perf_test.py` via `uv run`,
  or a Rust bench) — asserts nav-tree payload stays bounded.
- Add `make bench` (wraps `oha` + payload sizing) as a perf-regression gate.
- Verify the 4 `AWAITING_VERIFY` tasks against the live stack.

### Phase 1 — Kill the CDN (highest UX ROI, ADR-0007 compliance)
- Adopt **Tailwind v4 standalone CLI**: `static/input.css` → built
  `static/miku.tw.css`; add `make css` (devShell binary). Drop `@tailwindcss/browser`.
- **Vendor** Alpine, htmx, Prism (core+autoloader+theme) to `/static/js/vendor/`
  at pinned versions; make Mermaid lazy + index-flagged (`has_mermaid`) per ADR-0007.
- Add long-cache headers + content-hashed asset URLs.
- Outcome: zero third-party runtime origins, smaller render-blocking head,
  offline-capable, no in-browser CSS compile.

### Phase 2 — Shell decomposition + scale
- Split the 714-line `base.html` into partials; extract inline JS into vendored modules.
- Implement the **lazy sidebar** (`docs/lazy_sidebar_plan.md`) to remove the O(n) tree.
- Optional **valkey cache** (`docs/valkey_caching_plan.md`) — honor AGENTS.md
  "no persistent volume for transient cache" rule.

### Phase 3 — UX overhaul (adopt OSS patterns)
- Reading view: Quartz-style prose typography, breadcrumb, TOC, page-info rail.
- Editor: confirm CM6 split live-preview (SilverBullet pattern) — already integrated.
- Navigation: command palette / page picker (SilverBullet) via vendored Alpine.
- Theme: keep the teal→pink Miku identity; ensure light/dark via built CSS, not CDN.

### Verification (every phase)
e2e tests + `make bench` payload/latency gate + manual stack walkthrough
(`make stack-build` then click-through) — per the "green ≠ working" rule.

---

## Sources
- SilverBullet — https://github.com/silverbulletmd/silverbullet
- OSS Notion/markdown alternatives (2026) — https://toolindex.net/blog/open-source-notion-alternatives-2026
- Tailwind Play CDN (dev-only) — https://tailwindcss.com/docs/installation/play-cdn
- Tailwind CDN → optimised bundle — https://www.conroyp.com/articles/tailwind-cdn-to-production-optimised-css-bundle
- Tailwind v4 CDN setup notes — https://tailkits.com/blog/tailwind-css-v4-cdn-setup/
