# Quick start

This guide creates a small product catalog, writes two rows, and searches them.

## Option A: Docker Compose

Clone the repository, choose an API key, and start the service:

```bash
export FRANKENSTEINDB_API_KEY='replace-this-with-a-long-random-secret'
docker compose up --build -d
curl http://localhost:8080/health
```

The response should be similar to:

```json
{"status":"ok","version":"0.2.0"}
```

The Compose file stores the database in the named `frankensteindb-data` volume. `docker compose
down` keeps the volume; `docker compose down -v` permanently deletes it.

Set a reusable shell variable for the authenticated examples:

```bash
export FDB='http://localhost:8080'
export AUTH="Authorization: Bearer $FRANKENSTEINDB_API_KEY"
```

## Option B: build from source

You need a recent stable Rust toolchain.

```bash
cargo build --release --locked
FRANKENSTEINDB_API_KEY='replace-this-with-a-long-random-secret' \
  target/release/frankensteindb-server ./data --listen 127.0.0.1:8080
```

In another terminal, set `FDB` and `AUTH` as shown above.

## 1. Create a table

A table has exactly one non-nullable `Integer` or `Text` primary key. Text columns also choose an
analyzer.

```bash
curl --fail-with-body -X POST "$FDB/api/v1/tables" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "name": "products",
    "columns": [
      {
        "name": "id", "data_type": "Integer",
        "primary_key": true, "nullable": false, "analyzer": null
      },
      {
        "name": "title", "data_type": "Text",
        "primary_key": false, "nullable": false, "analyzer": "Default"
      },
      {
        "name": "category", "data_type": "Text",
        "primary_key": false, "nullable": false, "analyzer": "Raw"
      },
      {
        "name": "price", "data_type": "Real",
        "primary_key": false, "nullable": true, "analyzer": null
      },
      {
        "name": "tags", "data_type": "TextArray",
        "primary_key": false, "nullable": false, "analyzer": "Raw"
      }
    ]
  }'
```

Omitted `index` and `document_store` objects use safe defaults. See
[Schema and data types](Schema-and-data-types) before tuning them.

## 2. Insert rows

```bash
curl --fail-with-body -X POST "$FDB/api/v1/tables/products/rows" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"id":1,"title":"Wireless headphones","category":"audio","price":79.5,"tags":["wireless","sale"]}'

curl --fail-with-body -X POST "$FDB/api/v1/tables/products/rows" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"id":2,"title":"Wired studio headphones","category":"audio","price":129,"tags":["studio"]}'
```

Normal writes return only after the new Tantivy reader is published, so an immediate read sees
them.

## 3. Fetch one row

```bash
curl --fail-with-body "$FDB/api/v1/tables/products/rows/1" -H "$AUTH"
```

Single-row responses include an `ETag`. You can pass it as `If-Match` on `PUT`, `PATCH`, or
`DELETE` to reject concurrent overwrites.

## 4. Search

```bash
curl --fail-with-body -X POST "$FDB/api/v1/tables/products/query" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{
    "projection": [
      {"kind":"column","column":"id","alias":null},
      {"kind":"column","column":"title","alias":null},
      {"kind":"score","alias":"relevance"}
    ],
    "filter": {
      "kind":"search",
      "fields":["title"],
      "query":"headphones"
    },
    "order_by":[{"column":"_score","descending":true}],
    "limit":20
  }'
```

The response uses a predictable envelope:

```json
{
  "data": [
    {"id": 1, "title": "Wireless headphones", "relevance": 0.18},
    {"id": 2, "title": "Wired studio headphones", "relevance": 0.16}
  ],
  "meta": {
    "columns": ["id", "title", "relevance"],
    "count": 2,
    "limit": 20,
    "offset": 0
  }
}
```

Scores depend on the current index and are intentionally not stable constants.

## 5. Update safely

First fetch the row with response headers, copy its `ETag`, then patch it:

```bash
curl -i "$FDB/api/v1/tables/products/rows/1" -H "$AUTH"

curl --fail-with-body -X PATCH "$FDB/api/v1/tables/products/rows/1" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -H 'If-Match: "paste-etag-here"' \
  -d '{"price":69.5}'
```

A stale ETag returns HTTP `412 Precondition Failed`.

## Next steps

- Learn how analyzers and array types work in [Schema and data types](Schema-and-data-types).
- Add filters, highlights, and cursor pagination in [Search and queries](Search-and-queries).
- Import NDJSON and perform batch changes in [Writes, imports, and concurrency](Writes-imports-and-concurrency).
- Prepare a durable deployment with [Docker and deployment](Docker-and-deployment).
