# Indexer Scalability & Robustness Plan

Status: **Proposal (awaiting approval)** · Date: 2026-06-28 · Branch: `feat/mvp`
Relates: `docs/watcher_and_queue_plan.md`, ADR-0006, `docs/frontend_rework_plan.md`
(Phase 0a), pitfall `miku:pitfall:indexer-reindex-storm`.

All code references are `src/indexer.rs` unless noted.

---

## 1. Diagnosis (evidence-backed)

Reproduced by cloning `uaxe/geektime-docs` (**10,248 .md files**) into the vault.

Observed:
- `tb_pages` stayed **0** for ~5 minutes (index never became queryable).
- **84,101** `parsing/saving` operations for **8,879** unique files (~9.5× each).
- Repeating log cycle every few seconds:
  ```
  ERROR Reconciliation sweep failed: error returned from database:
        invalid byte sequence for encoding "UTF8": 0x00
  INFO  Reconcile details: 10253 updates, 0 deletions
  ```

Root cause chain:
1. **One poison file.** Exactly **1 of 10,248** files
   (`后端-架构/左耳听风/docs/11 - 程序中的错误处理….md`) contains a NUL byte
   (`0x00`). Postgres rejects NUL in `text`/`tsvector`, so its `INSERT` errors.
2. **Whole-vault single transaction.** `process_batch` opens one transaction
   (line 410), loops over *all* upserts, and commits once at the end (line 562).
   The poison file's error aborts the **entire** transaction → rollback →
   **0 pages committed**, even though 10,247 files were fine.
3. **Infinite retry of the identical failing batch.** A periodic reconcile fires
   every **5 seconds** (line 293) sending `__reconcile__`. Because nothing ever
   committed, every reconcile re-detects all 10,253 files as "new"
   (`Reconcile details: 10253 updates`) and re-runs the same doomed transaction,
   hitting the same NUL byte → rollback → repeat forever. That is the storm.

Contributing weaknesses (independent of the poison file):
- **No incremental commit / no batch cap** — even on clean data the whole vault
  is one transaction: slow, memory/lock-heavy, and invisible until commit.
- **No in-flight guard** — a new periodic reconcile can be queued while one is
  running; the 5s interval is far shorter than a full 10k scan, guaranteeing
  pile-up/overlap (also explains the 3 concurrent ACTIVE pg connections vs. the
  documented single-writer model).
- **`reconcile_all` is O(n) every tick** — walks the whole tree and
  `SELECT path, mtime` for all rows every 5s, even when idle (lines 578–609).
- **Unbounded SSE fan-out** — a full reconcile broadcasts one SSE message per
  affected page (lines 372–376, 659–661): 10k messages on a big sync.
- **Failed files are never quarantined** — a permanently-bad file is retried on
  every reconcile forever (its mtime is never recorded because the tx rolls back).

---

## 2. Fixes (prioritized)

### Critical — stops the storm + the 0-committed state

**F1 — Sanitize NUL / invalid bytes before insert.** Strip `\0` (and lone
invalid UTF-8 if any) from `body`, `title`, and stringified frontmatter prior to
binding (around lines 456–483). Cheap, defensive, fixes the immediate poison.

**F2 — Per-file (or small-batch) transaction isolation.** Replace the single
vault-wide transaction with one transaction **per file** (or per bounded chunk,
e.g. 100 files). On a per-file error: log, **skip that file**, and continue — one
bad document must never zero the index. (Refactor `process_batch` tx scope,
lines 410–562.)

**F3 — In-flight reconcile guard + backoff.** Track a "reconcile running" flag
(e.g. `AtomicBool` or a single-permit guard); the periodic ticker skips a tick if
a reconcile/consumer batch is already in progress (lines 291–300, 325–331). This
alone collapses the overlap/storm.

### High — scalability hardening

**F4 — Incremental commit.** Commit in bounded chunks so the index becomes
queryable *during* a large rescan and progress is visible (pairs with F2).

**F5 — Quarantine permanently-bad files.** Record files that fail even after
sanitization (in-memory set, or a `tb_index_errors` table) so they aren't
re-attempted every reconcile; surface them for the user. Prevents silent
infinite ret\-loops on a genuinely unparseable file.

**F6 — Relax + configure the reconcile interval.** 5s → 30–60s, env-configurable;
position it explicitly as the bind-mount fallback it already documents
(lines 285–300). The watcher remains the primary trigger.

**F7 — Bound/coalesce SSE on bulk sync.** On a full reconcile, skip per-page
broadcast above a threshold (or send a single "index refreshed" signal) instead
of N messages (lines 372–376, 659–661).

### Optional — throughput

**F8 — Bounded-concurrency parse.** Parse/extract markdown off the DB path with a
bounded worker pool, keeping a **single DB writer** (preserves the invariant) fed
by a channel. Defer until F1–F4 land.

---

## 3. Verification

- **Regression test:** add a fixture file containing a NUL byte; assert the
  indexer indexes the other fixtures and `tb_pages` count == good-file count
  (poison isolated, not fatal).
- **Scale test (`scripts/index_scale_test.py` via `uv run`, or a Rust test):**
  index the 10k corpus; assert (a) `tb_pages` converges to N, (b) total index
  operations ≈ N (no ~9× storm), (c) convergence under a time budget.
- **`make bench`** gate wrapping the scale test + `oha` page-serving run, so this
  cannot regress silently (the "`make check` green ≠ working" lesson).
- Manual: `podman compose down && up`, watch `tb_pages` climb and stabilize, and
  the storm logs disappear.

---

## 4. Suggested sequencing

1. F1 + F2 + F3 (critical trio) — one PR; turns a livelock into a converging
   index that tolerates bad files. Land with the NUL regression test.
2. F4 + F6 + F7 — scalability/UX hardening.
3. F5 — quarantine + surfacing.
4. F8 — throughput, only if needed after measuring.
