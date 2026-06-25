# Todo

Authoritative task state lives in asobi (`sedum:mvp:task-*`). Snapshot:

## In progress

- _(none — awaiting dispatch of task-1)_

## Ready

- **task-1** — Project skeleton + trimmed deps + config

## Blocked (by dependency)

- **task-2** — Markdown CRUD + render + atomic async save (needs task-1)
- **task-3** — Background indexer: notify → parse → Postgres (needs task-2)
- **task-4** — `[[wiki links]]` parse / resolve / render (needs task-3)
- **task-5** — Backlinks panel (needs task-4)
- **task-6** — Tags: `#tag` parse + filter/list (needs task-3)
- **task-7** — Full-text search, Postgres tsvector (needs task-3)

## Done

- Repo cleaned: purged openraft/redis/postgres-demo/tonic/quiche scaffold
- Stack + DB + frontend decisions locked
- Agent infra + docs initialized
