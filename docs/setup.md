# Setup

## Prerequisites

- Nix (with flakes) — the devShell provides rust, prettier, and uv
- Postgres (running locally or reachable via `DATABASE_URL`)

## Configure

Set the database URL (kept out of git):

```bash
export DATABASE_URL=postgres://localhost/miku
```

## Build, run, test

```bash
nix develop       # enter the devShell (provisions all tools)
make run          # run the server
make check        # fmt-check + lint + test (before commit)
make validate     # check + release build (before PR)
```

Project automation/scripts are Python run via `uv run python scripts/<x>.py`
(root `pyproject.toml`), not bash.

## Database

The Postgres index is a disposable cache. Migrations live under `migrations/`
and are applied via sqlx. The index is fully rebuildable from `miku/**/*.md` —
dropping and re-migrating the database loses no user data.
