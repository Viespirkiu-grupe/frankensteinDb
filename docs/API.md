# FrankensteinDB HTTP API

The server exposes typed JSON only. SQLite is private authoritative storage; rows, filters,
sorting, full-text search, ETags, and aggregations are read exclusively from published Tantivy
snapshots.

## Start and authenticate

```bash
cargo run --release --bin frankensteindb-server -- ./data --listen 127.0.0.1:8080
```

A legacy full-access key can come from `--api-key` or `FRANKENSTEINDB_API_KEY`. For multiple keys,
use `--api-key-config keys.json`:

```json
{
  "keys": [{
    "id": "reporting",
    "sha256": "<lowercase SHA-256 of a high-entropy token>",
    "scopes": ["read"],
    "tables": ["sutartys", "public_*"]
  }]
}
```

Optional `not_before` and `expires_at` values are RFC 3339 timestamps. Scopes are `read`, `write`,
`maintenance`, and `admin`; admin implies every scope. Reload the file with
`POST /api/v1/auth/reload`. Send credentials as `Authorization: Bearer <token>`.

`GET /api/v1/audit` (admin scope) returns the latest 1,000 mutation, maintenance, and failed-auth
audit records. Bodies and bearer values are never recorded.

`/health`, `/metrics`, and `/openapi.json` are public. Responses include `X-Request-Id`; supplied
request IDs are preserved. Errors always use `{"error":{"code":"...","message":"..."}}`.

## Tables and rows

Create and inspect tables:

```bash
curl -X POST localhost:8080/api/v1/tables -H 'Content-Type: application/json' -d '{
  "name":"products",
  "columns":[
    {"name":"id","data_type":"Integer","primary_key":true,"nullable":false,"analyzer":null,"compact_raw":false,"index":{"indexed":true,"record":"positions","stored":false}},
    {"name":"title","data_type":"Text","primary_key":false,"nullable":false,"analyzer":"Default","compact_raw":false,"index":{"indexed":true,"record":"positions","stored":false}},
    {"name":"price","data_type":"Real","primary_key":false,"nullable":true,"analyzer":null,"compact_raw":false,"index":{"indexed":true,"record":"positions","stored":false}}
  ]
}'
curl localhost:8080/api/v1/tables/products
```

`POST /rows` inserts. `PUT /rows/{key}` fully replaces all non-PK fields: every non-nullable field
is required and omitted nullable fields become null. `PATCH /rows/{key}` changes only supplied
fields. The path key is authoritative and cannot be changed.

```bash
curl -X POST localhost:8080/api/v1/tables/products/rows \
  -H 'Content-Type: application/json' -d '{"id":42,"title":"Headphones","price":79.5}'
curl -i localhost:8080/api/v1/tables/products/rows/42
curl -X PATCH localhost:8080/api/v1/tables/products/rows/42 \
  -H 'If-Match: "<etag-from-get>"' -H 'Content-Type: application/json' -d '{"price":69.5}'
```

`If-Match` is optional on single-row PUT, PATCH, and DELETE. A stale value returns `412`.
`?deferred=true` commits SQLite/outbox without publishing a new reader; `POST /api/v1/flush`
publishes deferred work.

## Typed reads and aggregations

```bash
curl -X POST localhost:8080/api/v1/tables/products/query \
  -H 'Content-Type: application/json' -d '{
    "projection":[{"kind":"column","column":"id","alias":null}],
    "filter":{"kind":"search","fields":["title"],"query":"headphones"},
    "order_by":[{"column":"_score","descending":true}],
    "limit":20,
    "offset":0
  }'
```

Filters also include `search_boosted`, `disjunction_max`, `fuzzy`, `prefix`, `phrase_prefix`,
`json_search`, `geo_distance`, and `geo_bounding_box`. `in` uses Tantivy's deduplicating
`TermSetQuery`, avoiding one boolean clause per
value for large sets. It has set/filter semantics and does not rank rows by how many requested
values matched. Structural `compare`, `between`, `in`, `is_null`, cursor, and negation filters are
wrapped in zero-valued Tantivy `ConstScoreQuery` nodes. They restrict matches without changing BM25;
a query containing only structural filters has `_score = 0`. `min_score` is optional.
Projection kind `highlight` accepts `column`, optional `alias`, and `fragment_size` (32–4096).
It uses Tantivy's `SnippetGenerator`, so fragment selection and match ranges use the field's actual
analyzer. Returned HTML is escaped and wraps matched tokens in `<b>`.
Independent requests execute concurrently through Tantivy. Send the same body to `/explain` for
the selected collector or `/profile` for compilation, count, materialization, segment, and total
timings.

JSON columns expose typed dynamic paths without reading SQLite. `json_compare`, `json_between`, and
`json_exists` accept a dotted `path`; typed operations also require `data_type`: `string`, `i64`,
`u64`, `f64`, `bool`, or `date_time`. RFC 3339 strings are the only JSON values Tantivy detects as
`date_time`. A mismatch between the requested and observed dynamic type is rejected instead of
silently coercing data:

```json
{
  "filter": {
    "kind":"json_between", "column":"metadata", "path":"price",
    "data_type":"f64", "lower":10, "upper":100
  },
  "order_by":[{
    "column":"metadata", "json_path":"rank", "json_type":"i64", "descending":false
  }]
}
```

`json_exists` may omit `data_type` to match any scalar type at the path. JSON path sorting does not
currently support `search_after`.

Geo points use `{"lat":54.6872,"lon":25.2797}`. A sort adds `geo_distance_from` and optional
`geo_distance_mode` (`min`, `max`, or `average`); projection kind `geo_distance` returns meters.
Geo cursors are supported. Aggregation kind `geo_tile_grid` accepts zoom 0–31, `max_buckets`,
`count_mode` (`documents` or `points`), and optional bounds for map heatmaps. Geo grids are local,
top-level aggregations and are not part of distributed intermediate payloads.

To explain why one hit received its score, send the query body to the primary-key resource:

```bash
curl -X POST localhost:8080/api/v1/tables/products/rows/42/explain-score \
  -H 'Content-Type: application/json' \
  -d '{"filter":{"kind":"search","fields":["title"],"query":"wireless headphones"}}'
```

The response contains Tantivy's recursive explanation tree, including boosts, term frequency,
inverse document frequency, field length normalization, and the final `score`. The selected row must
match the supplied query.

For deep pagination, use the `next_search_after` array returned in response metadata instead of
increasing `offset`. The first request needs an explicit non-nullable scalar sort:

```json
{
  "projection":[{"kind":"column","column":"id","alias":null}],
  "order_by":[{"column":"created_at","descending":true}],
  "limit":100
}
```

If another page exists, its response contains a cursor such as:

```json
{"meta":{"next_search_after":["2026-07-20T10:00:00+00:00",42]}}
```

Send that array unchanged in the next request as `"search_after"`. The primary key is appended to
the effective sort as a deterministic ascending tie-breaker, which explains the final cursor value.
Keep the same filter and `order_by` across pages.
`search_after` cannot be combined with `offset`, aggregations, `min_score`, score projection,
nullable/array sorts, `_score`, or unindexed sort columns. It is a live cursor rather than a
point-in-time snapshot: concurrent writes can change later pages.

```json
{
  "projection":[
    {"kind":"column","column":"id","alias":null},
    {"kind":"highlight","column":"title","alias":"snippet","fragment_size":160}
  ],
  "filter":{"kind":"search_boosted","fields":{"title":4,"description":1},"query":"wireless headphones"},
  "min_score":0.5,
  "limit":20
}
```

`prefix` provides single-token autocomplete and `fuzzy` accepts distance 0–2. Multi-word
autocomplete uses `phrase_prefix`; the final analyzed token is the prefix:

```json
{
  "kind":"phrase_prefix",
  "column":"title",
  "phrase":"public proc",
  "max_expansions":50
}
```

It requires `record: "positions"`, 2–32 analyzed tokens, and `max_expansions` from 1 to 4096.
`disjunction_max` searches each field independently and scores a row as its best field plus
`tie_breaker` times the remaining field scores:

```json
{
  "kind":"disjunction_max",
  "fields":{"title":4,"description":1},
  "query":"public procurement",
  "tie_breaker":0.1
}
```

It accepts 1–32 indexed text fields, positive boosts, and a `tie_breaker` from 0 to 1. A zero tie
breaker uses only the best field score. To preserve this scoring under Tantivy 0.26, disjunction-max
children bypass its sum-oriented term-union Block-WAND specialization; other queries retain their
normal pruning optimizations. `regex` matches any indexed token accepted by a regular expression
in one `Text` or `TextArray` column:

```json
{"kind":"regex","column":"title","pattern":"wire.*"}
```

It does not require positions and uses a constant score rather than BM25. Regex applies to a
complete analyzed term, not the original field value; use `.*text.*` for a substring. Patterns
beginning with `.*` can traverse a large part of the term dictionary and should be used carefully.

`regex_phrase` matches a sequence of regular expressions against token positions in one `Text` or
`TextArray` column:

```json
{
  "kind":"regex_phrase",
  "column":"title",
  "patterns":["wire.*","head.*"],
  "slop":0,
  "max_expansions":4096
}
```

It requires `record: "positions"`, 2–16 patterns, `slop` up to 64, and at most 16,384 total term
expansions per segment. Patterns target already analyzed index terms; they are not passed through the
field analyzer. Regex and prefix expansion terms are subject to what Tantivy exposes through the
compiled query's term visitor.

Find lexically similar rows from a Tantivy-only primary-key seed:

```bash
curl -X POST localhost:8080/api/v1/tables/products/rows/42/similar \
  -H 'Content-Type: application/json' -d '{
    "fields":["title"],
    "min_term_frequency":1,
    "min_doc_frequency":2,
    "max_query_terms":25,
    "limit":20
  }'
```

The seed row is excluded automatically. An optional typed `filter` restricts candidates without
changing similarity scores. Empty `fields` selects all indexed `Text` and `TextArray` columns.
Responses contain every table column plus `_score`. Supported seed fields are text, signed/unsigned
numeric, date/time, and facet scalar/array fields; this is lexical similarity, not vector search.

The optional `aggregations` object is a recursive named tree. Set `limit: 0` for an aggregation-only
request:

```json
{
  "limit": 0,
  "filter": {"kind":"compare","column":"active","operator":"equal","value":true},
  "aggregations": {
    "by_category": {
      "kind":"terms", "column":"category", "size":20,
      "aggregations": {
        "average_price":{"kind":"metric","function":"average","column":"price","percents":null},
        "price_percentiles":{"kind":"metric","function":"percentiles","column":"price","percents":[50,95,99]},
        "latest":{"kind":"top_hits","size":3,"sort":[{"column":"created_at","descending":true}],"columns":["id","created_at"]}
      }
    }
  }
}
```

Bucket kinds are `terms`, `histogram`, `date_histogram`, `range`, `filter`, and paginable
`composite` (pass the previous response's `after_key` as `after`). JSON fast fields use
`json_terms`, `json_histogram`, and `json_range`; metrics accept `json_path` instead of `column`.
Metrics are `count`,
`sum`, `average`, `min`, `max`, `stats`, `extended_stats`, approximate `cardinality`, and
`percentiles`; `top_hits` is allowed inside a bucket.
Trees are limited to depth 4, terms size 10,000, and top-hits size 100. Arrays are multi-valued
fields and can contribute one document to multiple buckets.

Terms accept `segment_size`, `min_doc_count`, `missing`, and an `order` object such as
`{"target":"_count","descending":true}`. Numeric and date histograms accept `offset`,
`min_doc_count`, `hard_bounds`, `extended_bounds`, and `keyed`; ranges accept `keyed` and then
require a key on every range. Metrics accept a typed `missing` value. Composite sources support
`missing_bucket` plus `missing_order` (`default`, `first`, or `last`). Composite date histograms
require exactly one of `fixed_interval` or `calendar_interval`; calendar values are `year`, `month`,
and `week`.

For distributed execution, each shard receives the same aggregation request at
`POST /api/v1/tables/{table}/aggregate-intermediate`. The returned `payload_hex` uses the versioned
`tantivy-bincode-v1` format and is intentionally opaque. Send all payloads and the identical
aggregation tree to `POST /api/v1/tables/{table}/aggregate-merge`:

```json
{
  "aggregations":{"by_category":{"kind":"terms","column":"category","size":20}},
  "payloads_hex":["...", "..."]
}
```

The merge endpoint rejects mismatched request hashes, unknown payload versions, more than 1,024
shards, or more than 256 MiB of decoded intermediate data.

Hierarchical `Facet` columns use `POST /api/v1/tables/{table}/facets/{column}` with
`{"root":"/services","limit":100,"filter":null,"exclude_own_filter":false}`. The response
counts direct children. Set `exclude_own_filter` to `true` for multi-select navigation: structural
`compare`, `between`, `in`, and `is_null` predicates on the requested facet column are removed while
filters on every other column remain active.

Replace a table's aliases with `PUT /api/v1/tables/{table}/aliases` and a JSON string array. Reads
and writes resolve aliases to the same published generation. Reindex builds separately and swaps the
committed generation before catalog publication, so existing readers remain usable throughout.

## Column types and index profiles

Types are `Integer`, `Unsigned`, `Real`, `Text`, `Boolean`, `Date`, `DateTime`, `Timestamp`,
`TextArray`, `IntegerArray`, `Blob`, `Ip`, `Json`, `Facet`, `GeoPoint`, and `GeoPointArray`. JSON values must be objects; facet
paths start with `/`. Unsigned values use SQLite TEXT internally to retain the full `u64` range.

Every scalar family has a multi-value form: `TextArray`, `IntegerArray`, `UnsignedArray`,
`RealArray`, `BooleanArray`, `DateArray`, `DateTimeArray`, `TimestampArray`, `BlobArray`, `IpArray`,
`JsonArray`, and `FacetArray`. Array filters match any contained value. JSON-array elements must be
objects, BLOB-array elements use the same `0x...` encoding, and facet-array elements start with `/`.

Every column accepts `index`: `indexed` controls queryability, `stored` controls Tantivy's compressed
document store, and text `record` is `basic`, `frequencies`, or `positions`. Projection stays
available because canonical values are fast fields. Phrase search requires `positions`; BM25
requires at least `frequencies`.

The table-level `document_store` object controls stored-field compression:

```json
{
  "compression":"zstd",
  "zstd_level":3,
  "block_size":65536,
  "dedicated_thread":true
}
```

`compression` is `lz4`, `zstd`, or `none`. Defaults are LZ4, a 16 KiB block, and a dedicated
compression thread. Zstd without `zstd_level` uses level 3. Changing these settings on an existing
table uses schema change `{"kind":"alter_document_store","document_store":{...}}`, which rebuilds
and atomically swaps the Tantivy generation. Compression only affects columns whose `index.stored`
is true; canonical projection values continue to use fast fields.

The `Custom` analyzer combines optional stemming, custom stop words, and ASCII folding:

```json
{"Custom":{"stem":"english","stop_words":["the","and"],"synonyms":{"tv":["television"]},"ascii_folding":true}}
```

## Safe filter mutations

Filter updates use PATCH; deletes wrap the filter in an object:

```bash
curl -X PATCH 'localhost:8080/api/v1/tables/products/rows?max_rows=1000' \
  -H 'Content-Type: application/json' \
  -d '{"filter":{"kind":"compare","column":"active","operator":"equal","value":false},"values":{"price":0}}'

curl -X DELETE 'localhost:8080/api/v1/tables/products/rows?dry_run=true' \
  -H 'Content-Type: application/json' \
  -d '{"filter":{"kind":"compare","column":"active","operator":"equal","value":false}}'
```

`dry_run` returns the Tantivy match count without writing. `max_rows` is checked against the exact
PK set already resolved by Tantivy before SQLite changes begin; no second count query or
confirmation token is used.

## Streaming import

`POST /api/v1/tables/{table}/imports` accepts NDJSON objects and performs chunked upsert:

```bash
curl -X POST 'localhost:8080/api/v1/tables/products/imports?batch_size=5000&on_error=abort' \
  -H 'Content-Type: application/x-ndjson' --data-binary @products.jsonl
zstd -c products.jsonl | curl -X POST localhost:8080/api/v1/tables/products/imports \
  -H 'Content-Type: application/x-ndjson' -H 'Content-Encoding: zstd' --data-binary @-
```

Supported encodings are identity, gzip, and zstd. `on_error=abort` is default;
`on_error=skip` skips malformed object/column rows and reports rejected count. The upload is spooled
with backpressure before a persistent import job starts.

## Schema changes and jobs

Schema changes are an array sent to `POST /api/v1/tables/{table}/schema-changes`:

```json
[
  {"kind":"rename_column","from":"title","to":"name"},
  {"kind":"add_column","column":{"name":"stock","data_type":"Integer","primary_key":false,"nullable":false,"analyzer":null,"compact_raw":false},"default":0}
]
```

Kinds are `add_column`, `drop_column`, `rename_column`, `alter_column`, and
`alter_document_store`. PK rename/type/drop is unsupported. Migration strictly converts every row
through a shadow SQLite table, builds a complete Tantivy generation, swaps it in with rollback
protection, and publishes the catalog. Existing mmap readers continue on the old generation; writes
return `423 Locked` until completion.

Reindex, optimize, schema change, import, and backup return `202` jobs. Use:

```text
GET    /api/v1/jobs
GET    /api/v1/jobs/{id}
DELETE /api/v1/jobs/{id}
POST   /api/v1/jobs/{id}/retry
GET    /api/v1/jobs/{id}/artifact
```

Job states survive restart. Jobs active during a crash become `interrupted`. Reindex, optimize, and
backup can be retried without resubmitting input; consumed import/schema input must be resubmitted.

Optimize accepts an optional JSON body:

```json
{"target_segments":8,"merge_threads":0}
```

It balances existing segments into at most `target_segments` disjoint groups and submits those
merges concurrently. `merge_threads: 0` uses up to four system-provided CPUs. Use
`target_segments: 1` for the previous full force-merge behavior. Optimization does not reindex or
split a table that already has fewer segments than requested.

## Backup and offline restore

`POST /api/v1/backups` creates a `.tar.zst` job artifact containing checkpointed SQLite, published
Tantivy indexes, and a SHA-256 manifest. Download it from the job artifact endpoint.

Stop the server before restore:

```bash
cargo run --release -- ./data restore frankensteindb-backup.tar.zst --force
```

Restore verifies format, sizes, checksums, and database recovery in a sibling staging directory
before atomically replacing the target.
