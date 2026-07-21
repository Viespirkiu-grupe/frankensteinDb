# Operations, security, and monitoring

## Server configuration

```bash
frankensteindb-server <database-directory> \
  --listen 0.0.0.0:8080 \
  --api-key-config /run/secrets/frankensteindb-keys.json
```

| Option or environment variable | Purpose |
| --- | --- |
| Positional database directory | Owns `data.sqlite3`, indexes, jobs, and artifacts |
| `--listen` | Socket address; default `127.0.0.1:8080` |
| `--api-key` / `FRANKENSTEINDB_API_KEY` | One full-access plaintext bearer token |
| `--api-key-config` / `FRANKENSTEINDB_API_KEY_CONFIG` | File containing hashed scoped keys |

If neither key option is configured, protected endpoints are unauthenticated. This is convenient for
local development but unsafe on an exposed network.

## Scoped API keys

Use a high-entropy random token. Store only its lowercase SHA-256 digest:

```bash
TOKEN="$(openssl rand -hex 32)"
printf '%s' "$TOKEN" | sha256sum
```

Example configuration:

```json
{
  "keys":[
    {
      "id":"reporting",
      "sha256":"paste-lowercase-sha256-here",
      "scopes":["read"],
      "tables":["products","public_*"]
    },
    {
      "id":"operator",
      "sha256":"another-lowercase-sha256",
      "scopes":["read","write","maintenance"],
      "tables":["*"]
    }
  ]
}
```

Optional `not_before` and `expires_at` values use RFC 3339. Available scopes are:

| Scope | Allows |
| --- | --- |
| `read` | Tables, rows, queries, aggregations, diagnostics, and facets |
| `write` | Row and batch mutations, imports, and flush |
| `maintenance` | Reindex, optimize, schema changes, backups, and jobs |
| `admin` | Every scope plus authentication reload and audit access |

`admin` implies all other scopes. `tables` is an allowlist supporting exact names and simple
prefix patterns such as `public_*`.

Send the raw token, not its digest:

```http
Authorization: Bearer <raw-token>
```

Reload a changed key file without restarting:

```bash
curl -X POST "$FDB/api/v1/auth/reload" -H "$AUTH"
```

On Unix, sending `SIGHUP` performs the same reload.

## Public endpoints

These endpoints do not require authentication:

- `GET /health` — liveness and package version
- `GET /metrics` — Prometheus text format
- `GET /openapi.json` — OpenAPI 3.1 contract

Put TLS in a trusted reverse proxy or service mesh. Bearer tokens must not cross an unencrypted
network.

## Prometheus metrics

`GET /metrics` exposes counters and gauges including:

- `frankensteindb_http_requests_total`
- `frankensteindb_http_errors_total`
- `frankensteindb_http_active_requests`
- `frankensteindb_http_request_duration_seconds_total`
- `frankensteindb_writer_operations_total`
- `frankensteindb_writer_active`
- `frankensteindb_writer_wait_seconds_total`
- `frankensteindb_writer_duration_seconds_total`
- `frankensteindb_jobs_active`
- `frankensteindb_outbox_records`
- `frankensteindb_tables`
- `frankensteindb_tantivy_segments`
- `frankensteindb_tantivy_documents`

Alert on a growing outbox, sustained writer wait time, job failures, or unexpected segment growth.
The duration metrics are totals; derive rates in Prometheus.

## Audit records

`GET /api/v1/audit` requires `admin` and returns the latest 1,000 mutation, maintenance, and failed
authentication records. Request bodies and bearer tokens are never stored in the audit log.

Every HTTP response includes `X-Request-Id`. Supply your own ID to carry a trace identifier across
the boundary.

## Persistent jobs

Imports, schema changes, reindex, optimize, and backup run as jobs:

```text
GET    /api/v1/jobs
GET    /api/v1/jobs/{id}
DELETE /api/v1/jobs/{id}
POST   /api/v1/jobs/{id}/retry
GET    /api/v1/jobs/{id}/artifact
```

Jobs survive restart. A job that was running when the process stopped becomes `interrupted`.
Reindex, optimize, and backup jobs can be retried. Import and schema input is consumed and must be
submitted again.

Cancellation is cooperative. Poll the job record until its state changes.

## Reindex and optimize

```http
POST /api/v1/tables/{table}/reindex
POST /api/v1/tables/{table}/optimize
```

The optimize request body is optional. Defaults are:

```json
{"target_segments":8,"merge_threads":0}
```

- **Reindex** rebuilds derived Tantivy data from authoritative SQLite and atomically publishes the
  new generation.
- **Optimize** balances searchable segments into at most eight retained segments by default and
  merges independent groups concurrently. `merge_threads: 0` selects up to four available CPUs;
  set `target_segments: 1` for a full force merge. It does not reindex or split existing segments.
  Merging uses substantial temporary disk I/O and should not run after every write.

Existing reader snapshots remain usable while a new generation is built.

## Backups

Start a portable backup:

```bash
curl -X POST "$FDB/api/v1/backups" -H "$AUTH"
```

When the job completes, download `/api/v1/jobs/{id}/artifact`. The `.tar.zst` contains checkpointed
SQLite, published Tantivy indexes, and a SHA-256 manifest.

Restore while the server is stopped:

```bash
frankensteindb ./data restore frankensteindb-backup.tar.zst --force
```

Restore validates archive format, sizes, checksums, and database recovery in a sibling staging
directory before replacing the target. Keep off-host copies and regularly test that they restore.

## Admin CLI

The CLI accepts typed JSON files; `-` reads JSON from standard input:

```bash
frankensteindb ./data tables
frankensteindb ./data create table.json
frankensteindb ./data read request.json
frankensteindb ./data mutate mutation.json
frankensteindb ./data mutate mutation.json --deferred
frankensteindb ./data flush
frankensteindb ./data reindex products
frankensteindb ./data optimize products
frankensteindb ./data optimize products --target-segments 1 --merge-threads 4
frankensteindb ./data drop products
```

Do not run the CLI against the same directory while the HTTP server owns it. Prefer the HTTP API
for live administration.

## Upgrade checklist

1. Create and download a backup.
2. Read release notes for format or API changes.
3. Restore the backup into a separate directory and test representative queries.
4. Stop the old process cleanly.
5. Start the new image against the real volume.
6. Check `/health`, outbox size, job state, and representative read/write requests.
7. Keep the previous image digest available for rollback.
