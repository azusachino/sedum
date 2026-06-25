---
paths:
  - "**/*_test.rs"
  - "tests/**"
  - "src/**/tests.rs"
---

# Testing conventions

- Unit tests live in a `#[cfg(test)] mod tests` block beside the code
- Integration tests live under `tests/`
- DB-backed tests use `#[sqlx::test]` against a disposable test database;
  never point tests at a real vault's Postgres
- Indexer tests: write a temp `.md` file, assert the index reflects it after a
  reindex — the filesystem is the input, Postgres is the assertion target
- Run via `make test` (never `cargo test` directly)
