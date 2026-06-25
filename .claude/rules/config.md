## Agent Rules — Config & Migrations

### DO

- Keep Postgres connection config in env (`DATABASE_URL`); never hardcode
- Add schema changes as new sqlx migrations under `migrations/` — never edit an
  applied migration
- Treat the Postgres index as rebuildable: a migration may drop/recreate index
  tables since they are caches, not source of truth
- Document every config key change in the commit message

### DON'T

- Commit secrets or credentials (`.env`, `dev.env` stay out of git history)
- Remove a config key without checking all consumers
- Store anything in Postgres that cannot be regenerated from `sedum/**/*.md`
