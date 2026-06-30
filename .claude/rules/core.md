## Agent Rules — Core

### DO

- Use `make <target>` for all task execution — never run tools directly
- At session start: load asobi entities (`/asobi start`)
- At session end: write state to the `miku:session` asobi entity; save
  conventions to the `miku` project entity (`/asobi end`)
- Dispatch sub-agents for independent tasks — parallelize where possible
- Stage files explicitly: `git add <specific files>` only
- Keep the core invariant: files under `miku/` are truth; Postgres is a
  disposable, rebuildable index

### DON'T

- Commit or push without user confirmation
- Use `git add -A` or `git add .`
- Install tools globally — tools come from the Nix devShell (`nix develop`)
- Write bash glue for automation — use Python via `uv run` instead
- Lint or format `.md` files (markdown linting is disabled)
- Index inside the save handler — the `notify` watcher is the sole trigger
- Recompute the whole graph while typing; load full backlink/edge sets at once
