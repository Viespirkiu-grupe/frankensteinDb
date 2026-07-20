# Writes, imports, and concurrency

FrankensteinDB supports resource-style row operations, filter-based mutations, atomic batches, and
streaming imports. Choose the smallest API that matches the job.

## Single-row operations

### Insert

```http
POST /api/v1/tables/products/rows
Content-Type: application/json

{"id":42,"title":"Headphones","price":79.5}
```

The body is a column-name object. Unknown columns and wrong JSON types are rejected. Missing
nullable columns become `null`; missing required columns fail validation.

### Replace

```http
PUT /api/v1/tables/products/rows/42
Content-Type: application/json

{"title":"Studio headphones","price":129.0}
```

`PUT` replaces every non-primary-key field. All non-nullable fields are required, and omitted
nullable fields become `null`. The path key is authoritative; do not include a conflicting key in
the body.

### Patch

```http
PATCH /api/v1/tables/products/rows/42
Content-Type: application/json

{"price":99.0}
```

`PATCH` changes only the supplied fields.

### Delete

```http
DELETE /api/v1/tables/products/rows/42
```

## Optimistic concurrency with ETags

`GET /rows/{key}` includes a strong `ETag` computed from the canonical row. Supply it on a later
`PUT`, `PATCH`, or `DELETE`:

```bash
curl -i "$FDB/api/v1/tables/products/rows/42" -H "$AUTH"

curl -X PATCH "$FDB/api/v1/tables/products/rows/42" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -H 'If-Match: "copied-etag"' \
  -d '{"price":99}'
```

If another writer changed the row first, the operation returns `412 Precondition Failed`. Omitting
`If-Match` requests last-write-wins behavior.

## Filter-based update and delete

Update all rows matching a Tantivy filter:

```bash
curl -X PATCH "$FDB/api/v1/tables/products/rows?max_rows=1000" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "filter":{"kind":"compare","column":"active","operator":"equal","value":false},
    "values":{"price":0}
  }'
```

Delete by filter:

```bash
curl -X DELETE "$FDB/api/v1/tables/products/rows?dry_run=true&max_rows=1000" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "filter":{"kind":"compare","column":"active","operator":"equal","value":false}
  }'
```

Safety options:

- `dry_run=true` resolves and reports the match count without writing.
- `max_rows=N` rejects the mutation when the exact resolved primary-key set is larger than `N`.
- Both use the same Tantivy match set that the real mutation would consume.

## Atomic mutation batches

Send typed insert, update, and delete operations to `POST /api/v1/mutations`:

```json
[
  {
    "kind":"insert",
    "table":"products",
    "row":{"id":43,"title":"USB headset","price":49}
  },
  {
    "kind":"update",
    "table":"products",
    "values":{"active":false},
    "filter":{"kind":"compare","column":"id","operator":"equal","value":42}
  }
]
```

SQLite applies the batch atomically. All update/delete filters are resolved against the published
Tantivy snapshot before the transaction begins, so earlier batch items do not alter later filter
membership.

## Deferred writes

Append `?deferred=true` to supported row writes to commit SQLite without publishing a new Tantivy
reader after every request:

```bash
curl -X POST "$FDB/api/v1/tables/products/rows?deferred=true" ...
curl -X POST "$FDB/api/v1/flush" -H "$AUTH"
```

Use this for controlled batches where temporary stale reads are acceptable. Deferred rows are
durable but invisible to reads until flush. Always flush before reporting the batch complete.

## NDJSON imports

One JSON object per line:

```text
{"id":1,"title":"First","price":10}
{"id":2,"title":"Second","price":20}
```

Upload it:

```bash
curl -X POST \
  "$FDB/api/v1/tables/products/imports?batch_size=5000&on_error=abort" \
  -H "$AUTH" -H 'Content-Type: application/x-ndjson' \
  --data-binary @products.jsonl
```

Compressed streams are supported:

```bash
zstd -c products.jsonl | curl -X POST \
  "$FDB/api/v1/tables/products/imports?batch_size=5000" \
  -H "$AUTH" -H 'Content-Type: application/x-ndjson' \
  -H 'Content-Encoding: zstd' --data-binary @-
```

Supported `Content-Encoding` values are identity, `gzip`, and `zstd`.

| Option | Meaning |
| --- | --- |
| `batch_size` | Rows processed per chunk; default 5,000, maximum 50,000 |
| `on_error=abort` | Stop on the first malformed or invalid row |
| `on_error=skip` | Skip invalid rows and report the rejected count |

The request is spooled with backpressure, then a persistent background job performs the import.
The initial response is `202 Accepted`; monitor the returned job ID.

## Import performance checklist

- Prefer one streaming import over millions of HTTP row requests.
- Use a batch size that fits comfortably in memory; 5,000 is the safe starting point.
- Avoid `stored: true` unless required.
- Use document-store compression `none` only when stored fields exist and write speed matters more
  than disk size.
- Keep the database on local storage with reliable fsync behavior.
- Run `optimize` after a large one-off import only if lower segment count helps the real workload;
  it is not required after every batch.

## Error responses

Errors are always JSON:

```json
{"error":{"code":"bad_request","message":"column price requires a number"}}
```

Responses also include `X-Request-Id`. If a client supplies that header, the server preserves it,
which makes application and server logs easier to correlate.
