# Design Decisions (ADRs)

Resolutions to the open design corners. Each is a decision, not code. Mirrored
in asobi (`sedum:decision:*`). See `architecture.md` for the prose design.

---

## ADR-1 — Full-text search (English)

**Decision.** Use Postgres' built-in **`english`** FTS config:
`to_tsvector('english', title ‖ body)` with **title weighted A, body B**, ranked
by `ts_rank`, snippets via `ts_headline('english', …)`. Content and titles are
English in practice; the app name (麒麟草) is branding, not a content-language
requirement.

**Why this is now trivial.** No extension, no Rust tokenizer, no spike. Postgres
stays **vanilla** so the index is disposable/portable and identical across
compose and k8s. This was the only ADR that needed a spike — it's eliminated.

**Deferred, not rejected.** If meaningful CJK content ever shows up, revisit with
app-side `lindera` tokenization or `pgroonga`. Switching is just a reindex —
cheap, because the index is fully rebuildable from files.

---

## ADR-2 — Markdown & wikilink grammar

**Renderer — `comrak`.** `comrak` (Rust cmark-gfm) provides, as built-in
extension options: GFM (tables, strikethrough, task lists, footnotes,
autolinks), **native wikilinks** (`[[Target|Display]]`), and **native GitHub
alerts** (`> [!NOTE]` callouts). That keeps custom grammar work to two
extractors — `#tags` and `![[embed]]` — that the renderer does not provide
natively. Render speed is acceptable for a personal wiki, and rendered HTML can
be cached by content hash later if needed. No MDX/JSX (rejected — JS runtime,
breaks portability; `sedum:decision:no-mdx`).

**Editor vs renderer — CM6 is a separate axis.** CodeMirror 6 is a *client-side
editor*; it does **not** render the read view or feed the index — the server
still parses Markdown (comrak) for both. What CM6 buys is the *editing*
experience: in-editor syntax highlighting and a natural home for `[[ ]]`
autocomplete and live preview. So: **comrak for server-side render/index now;
CM6 for the editor (roadmap)** — orthogonal, not either/or.

**Tag grammar.** `#tag` where tag = Unicode letters/digits plus `_ - /` (so
hierarchical `#area/health` works). Recognized only in text (not code, URL
fragments, or `# ATX headings`); `#` immediately followed by a tag char, no
space. Stored whole (`area/health`); ancestor grouping (`area`) is a
**query-time prefix match**, not index-time expansion.

**Callouts — conflict resolved:** Obsidian/GitHub `> [!type]` wins (now *native*
via comrak alerts); `:::` directive syntax dropped; `architecture.md` updated.
**Transclusion** `![[Page]]` / `![[image.png]]`: custom, server-side, with a
recursion/cycle depth limit (roadmap render; grammar fixed now).

---

## ADR-3 — Write conflicts & auth

**Write conflicts.** Saves are atomic rename = last-write-wins, so two tabs (or
a browser edit racing a `git pull`) can silently clobber. **Decision: optimistic
concurrency.** The edit view embeds the file's **content hash** as a hidden
field; `POST` recomputes the hash before renaming. If it changed since load →
**409 Conflict** with a "file changed underneath you" prompt (show both
versions). Cheap (one read+hash before rename), guards the *file*, and never
involves the indexer — consistent with single-writer. Hash over mtime (mtime is
coarse and lies across `git`/`rsync`).

**On CRDTs (`loro-dev/loro`) — considered, deferred.** Loro is excellent
rich-text CRDT tech, but it solves *concurrent multi-writer / offline merge* — a
problem Sedum deliberately doesn't have (single-user, single-writer). The deeper
blocker: it **conflicts with files-are-truth**. A CRDT's authoritative state is
its operation log, which cannot be reconstructed from a plain `.md` snapshot, so
adopting it forces a fork — either the oplog becomes canonical (breaking "just
Markdown files you own") or we keep two sources of truth and must pick a winner.
**Decision: keep the content-hash 409.** Loro becomes the right choice *only if*
we commit to real-time collaborative editing, at which point it reshapes the
core invariant and earns its own ADR. Not a single-user need.

**Auth — no user system.** Sedum stays **single-user and login-less**; network
protection is the *deployment's* job. Two modes cover every persona:
- **`SEDUM_READONLY`** (roadmap flag) — serves view-only, no edit/save routes.
  For publishing; no auth needed because nothing writes.
- **Writable network deploy** — put Sedum behind an **authenticating reverse
  proxy** (oauth2-proxy / basic auth / Tailscale). Documented, not built.

Building accounts/RBAC is explicitly rejected — it reinvents Notion and breaks
"keep it simple." Local-first personas bind to localhost and need nothing.

---

## ADR-4 — Rename/delete & assets

**Rename = first-class operation, not a bare file move.** A bare rename leaves
every `[[OldName]]` dangling. **Decision:** `POST /page/rename` (1) atomically
renames the file, (2) finds referrers via `tb_links.target_id`, (3) rewrites
`[[Old]]→[[New]]` in each referrer through the normal atomic-save path
(preserving display aliases: `[[Old|Disp]]→[[New|Disp]]`). Each rewrite fires
`notify` → normal reindex. Honest caveat: this is the **one operation that
writes many files** — best-effort at the FS level; partial failures are
self-healed by the startup reconcile. UX: a "this will update N backlinks"
confirmation before committing.

**Reindex is automatic for both.** `notify` is the sole index trigger
(architecture invariant), so the rename's link-rewrites and the file move each
fire fs events → normal reindex. No handler indexes directly.

**Delete = soft-delete with a 7-day archive.** Deleting never `rm`s immediately.
The file is **moved to `sedum/.trash/<original-path>@<deleted-at>.md`**, and
`.trash/` is **excluded from the watcher and index** — so the page vanishes from
search/backlinks at once (its row is removed; inbound links go dangling via
`ON DELETE SET NULL`), while the bytes survive. A periodic GC purges trash
entries older than **`SEDUM_TRASH_TTL` (default 7 days)**. **Restore** = move the
file back → `notify` → reindex. Trash lives *inside* the content root (so it
travels with backups and the k8s PVC) but is ignore-listed, keeping the live
index pure. UX: a "N backlinks will dangle" warning before deleting.

**Assets.** Live in `sedum/assets/`. Upload (roadmap): `POST /assets` writes
atomically, keeping the original name but **deduping by content hash**
(`name-<short-hash>.ext`) to avoid collisions. `![[image.png]]` resolves by
basename in `assets/`; served with caching headers.

**Orphan assets — never auto-deleted.** Auto-GC of user files violates
files-are-truth (an asset may be referenced from outside Sedum, or kept
deliberately). Instead, a **report**: assets on disk minus
`tb_links(kind = 'asset' AND is_embed)` = unreferenced. Manual cleanup only. No
new table — asset targets already live in `tb_links`.
