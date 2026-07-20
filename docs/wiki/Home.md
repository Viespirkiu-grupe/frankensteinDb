# FrankensteinDB

FrankensteinDB is a search-first database for applications that need ordinary typed records and
powerful search in one process. Every write is stored durably in SQLite and indexed incrementally
in Tantivy. Every supported public read runs through Tantivy—there is no hidden SQLite fallback.

This gives you a deliberately small system with two complementary strengths:

- SQLite provides transactions, durable canonical rows, and crash recovery.
- Tantivy provides BM25 text search, filters, sorting, facets, JSON paths, and aggregations.

FrankensteinDB exposes a typed JSON HTTP API. It does not expose SQL and does not include a web UI.

> [!IMPORTANT]
> FrankensteinDB is currently an MVP. Back up important data, test upgrades on a copy, and review
> the documented limitations before using it for production workloads.

## Start here

If you want to try the server in a few minutes, follow [Quick start](Quick-start).

If you are designing a real schema, read these next:

1. [Core concepts and guarantees](Core-concepts-and-guarantees)
2. [Schema and data types](Schema-and-data-types)
3. [Search and queries](Search-and-queries)
4. [Writes, imports, and concurrency](Writes-imports-and-concurrency)

For deployment and maintenance:

- [Docker and deployment](Docker-and-deployment)
- [Operations, security, and monitoring](Operations-security-and-monitoring)
- [HTTP API reference](HTTP-API-reference)

## What it can do

- Typed scalar and multivalue array columns
- BM25 full-text search and natural Tantivy query syntax
- Exact, range, set, null, Boolean, and JSON-path filters
- Boosted multi-field and best-field search
- Prefix, phrase-prefix, fuzzy, regex, and positional regex-phrase queries
- Stable `search_after` pagination
- Tantivy-aware highlighting and score explanations
- More-like-this search and hierarchical facets
- Metrics, terms, range, histogram, date, composite, filter, and top-hits aggregations
- Incremental inserts, updates, deletes, and atomic mutation batches
- Streaming NDJSON imports with gzip and zstd support
- Schema migrations, aliases, reindexing, segment optimization, and portable backups
- Optional bearer authentication, scoped API keys, audit records, and Prometheus metrics

## Mental model

```text
HTTP or Rust request
        |
        +-- write --> SQLite transaction + durable outbox --> Tantivy writer --> published reader
        |
        `-- read  -------------------------------------------> published Tantivy reader
```

SQLite is authoritative, but it is private implementation storage. Do not open or modify
`data.sqlite3` while FrankensteinDB is running. Tantivy indexes are derived data and can be rebuilt
from SQLite with a reindex job.

## Naming

The project name is written **FrankensteinDB**. Package, executable, image, environment-variable,
and metric names use the conventional machine-readable forms:

| Place | Name |
| --- | --- |
| Rust crate | `frankensteindb` |
| Admin CLI | `frankensteindb` |
| HTTP server | `frankensteindb-server` |
| Benchmark | `frankensteindb-benchmark` |
| Container image | `ghcr.io/<owner>/frankensteindb` |
| Environment prefix | `FRANKENSTEINDB_` |
| Prometheus prefix | `frankensteindb_` |
