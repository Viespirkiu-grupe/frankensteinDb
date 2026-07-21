# HTTP API reference

Base URL: `http://host:8080`. Protected endpoints use `Authorization: Bearer <token>` when
authentication is configured. JSON request bodies require `Content-Type: application/json`.

The complete machine-readable OpenAPI 3.1 contract is served at `GET /openapi.json` and stored in
the repository as `docs/openapi.json`.

## Response conventions

Resource response:

```json
{"data":{}}
```

Row-list response:

```json
{
  "data":[],
  "meta":{"columns":[],"count":0,"limit":100,"offset":0}
}
```

Error response:

```json
{"error":{"code":"not_found","message":"table not found: products"}}
```

All responses include `X-Request-Id`. Common statuses are `200`, `201`, `202`, `204`, `400`, `401`,
`403`, `404`, `409`, `412`, `423`, `429`, and `500`.

## Public endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/health` | Liveness and version |
| `GET` | `/metrics` | Prometheus metrics |
| `GET` | `/openapi.json` | OpenAPI contract |

## Tables

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/v1/tables` | List table definitions |
| `POST` | `/api/v1/tables` | Create a table from `TableDef` |
| `GET` | `/api/v1/tables/{table}` | Get one table definition |
| `DELETE` | `/api/v1/tables/{table}` | Drop the table and index |
| `PUT` | `/api/v1/tables/{table}/aliases` | Replace aliases with a JSON string array |

## Rows and queries

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/v1/tables/{table}/rows` | List rows with query parameters |
| `POST` | `/api/v1/tables/{table}/rows` | Insert a row |
| `PATCH` | `/api/v1/tables/{table}/rows` | Update rows matching `UpdateBody` |
| `DELETE` | `/api/v1/tables/{table}/rows` | Delete rows matching `DeleteBody` |
| `GET` | `/api/v1/tables/{table}/rows/{key}` | Fetch one row and ETag |
| `PUT` | `/api/v1/tables/{table}/rows/{key}` | Replace one row |
| `PATCH` | `/api/v1/tables/{table}/rows/{key}` | Patch one row |
| `DELETE` | `/api/v1/tables/{table}/rows/{key}` | Delete one row |
| `POST` | `/api/v1/tables/{table}/query` | Execute a typed Tantivy read and aggregations |

Single-row writes accept optional `If-Match`. Supported writes accept `?deferred=true`.
Filter-based writes accept `dry_run` and `max_rows` query parameters.

## Search features and diagnostics

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/v1/tables/{table}/explain` | Explain query execution plan |
| `POST` | `/api/v1/tables/{table}/profile` | Execute with phase timings |
| `POST` | `/api/v1/tables/{table}/rows/{key}/explain-score` | Explain one hit's BM25 score |
| `POST` | `/api/v1/tables/{table}/rows/{key}/similar` | More-like-this search |
| `POST` | `/api/v1/tables/{table}/facets/{column}` | Count direct facet children |

Facet body:

```json
{"root":"/catalog","limit":100,"filter":null,"exclude_own_filter":false}
```

With `exclude_own_filter:true`, the requested facet ignores its own structural filter clauses but
keeps every other dimension's filter. This lets a selected facet continue showing viable
alternatives and their counts. The default is `false` for backward compatibility.

## Aggregation distribution

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/v1/tables/{table}/aggregate-intermediate` | Collect opaque shard fruit |
| `POST` | `/api/v1/tables/{table}/aggregate-merge` | Merge shard fruits into final JSON |

The merge body contains the same `aggregations` tree and one or more `payloads_hex` values.

## Bulk writes and imports

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/v1/mutations` | Apply an atomic typed mutation array |
| `POST` | `/api/v1/flush` | Publish deferred mutations |
| `POST` | `/api/v1/tables/{table}/imports` | Start an NDJSON import job |

Import query parameters are `batch_size` and `on_error=abort|skip`. Content encodings are identity,
gzip, and zstd.

## Maintenance and jobs

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/v1/tables/{table}/schema-changes` | Start shadow-table migration |
| `POST` | `/api/v1/tables/{table}/reindex` | Rebuild Tantivy from SQLite |
| `POST` | `/api/v1/tables/{table}/optimize` | Merge to `target_segments` (default 8) with configurable concurrency |
| `POST` | `/api/v1/backups` | Create portable backup artifact |
| `GET` | `/api/v1/jobs` | List jobs |
| `GET` | `/api/v1/jobs/{id}` | Get a job |
| `DELETE` | `/api/v1/jobs/{id}` | Request cancellation |
| `POST` | `/api/v1/jobs/{id}/retry` | Retry a retryable job |
| `GET` | `/api/v1/jobs/{id}/artifact` | Download a completed artifact |

Maintenance starts return `202 Accepted` and a job in the `data` envelope.

## Security and audit

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/api/v1/auth/reload` | Reload hashed-key configuration |
| `GET` | `/api/v1/audit` | Return latest audit records |

## Request model index

| Model | Documentation |
| --- | --- |
| `TableDef`, `ColumnDef`, `IndexProfile` | [Schema and data types](Schema-and-data-types) |
| `ReadBody`, `Projection`, `Filter`, `Sort` | [Search and queries](Search-and-queries) |
| `Mutation`, import options | [Writes, imports, and concurrency](Writes-imports-and-concurrency) |
| `Aggregation`, `CompositeSource`, `Metric` | [Aggregations and analysis](Aggregations-and-analysis) |
| Auth keys, jobs, backup | [Operations, security, and monitoring](Operations-security-and-monitoring) |

When implementing a generated client, use `/openapi.json` as the contract and these Wiki pages for
behavioral guidance and operational caveats.
