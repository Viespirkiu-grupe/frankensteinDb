# FrankensteinDB

FrankensteinDB is a typed embedded database that stores authoritative rows in SQLite and projects
every supported field into Tantivy. Reads, filters, sorting, full-text search, and aggregations run
exclusively through Tantivy. SQLite is used only for durable storage, transactions, and recovery.

The public Rust API, command-line interface, HTTP API, benchmarks, and tests expose no SQL.
Requests are serializable Rust types; full-text filter strings use Tantivy's search syntax.

## Documentation

The user guide is organized as a GitHub Wiki-ready set of pages in
[docs/wiki/Home.md](docs/wiki/Home.md). Start with:

- [Quick start](docs/wiki/Quick-start.md)
- [Schema and data types](docs/wiki/Schema-and-data-types.md)
- [Complete feature and limit reference](docs/wiki/Complete-feature-and-limit-reference.md)
- [Search and queries](docs/wiki/Search-and-queries.md)
- [Writes, imports, and concurrency](docs/wiki/Writes-imports-and-concurrency.md)
- [Docker and deployment](docs/wiki/Docker-and-deployment.md)
- [Embedding in Rust](docs/wiki/Embedding-in-Rust.md)
- [HTTP API reference](docs/wiki/HTTP-API-reference.md)

The exact OpenAPI 3.1 contract is available at [docs/openapi.json](docs/openapi.json) and from a
running server at `GET /openapi.json`.

## Docker quick start

```bash
export FRANKENSTEINDB_API_KEY='replace-with-a-long-random-secret'
docker compose up --build -d
curl http://localhost:8080/health
```

The production image uses an Alpine/musl runtime, runs as non-root, and stores all persistent state
in `/data`. See [Docker and deployment](docs/wiki/Docker-and-deployment.md) for GHCR tags, volumes,
backup guidance, and production recommendations.

## Build

```bash
cargo build --release
cargo test --all-targets
```

Do not run the large dataset benchmark as part of routine verification.

## Typed data model

Create tables with `TableDef`. Exactly one `Integer` or `Text` column must be the primary key.
Supported column types are `Integer`, `Unsigned`, `Real`, `Text`, `Boolean`, `Date`, `DateTime`,
`Timestamp`, `TextArray`, `IntegerArray`, `Blob`, `Ip`, `Json`, and hierarchical `Facet`.
The model also supports `GeoPoint` and `GeoPointArray`, with exact radius/bounds queries, distance
projection and sorting, cursor paging, and dynamic zoom 0–31 map-tile aggregation.
All scalar families also have an `Array` variant, including numeric, boolean, temporal, BLOB, IP,
JSON-object, and facet arrays. Tantivy indexes them as native repeated values and retains the
canonical array in a fast field.

Text and text-array columns require one analyzer:

- `Default`
- `Raw`
- `Whitespace`
- `{ "Stem": "english" }`
- `{ "Ngram": { "min": 2, "max": 4, "prefix_only": false } }`
- `{ "Custom": { "stem": "english", "stop_words": ["the"], "synonyms": {"tv": ["television"]}, "ascii_folding": true } }`

Each column has an `IndexProfile`: filter-only (`basic`), BM25-capable (`frequencies`), or
phrase-capable (`positions`), with optional compressed document storage. Values remain fast fields
because supported reads never fall back to SQLite.

Table `document_store` settings select `lz4` (default), `zstd`, or `none`, plus block size and a
dedicated compression thread. The default is LZ4 with 16 KiB blocks. Zstd defaults to level 3.

`DateTime` uses RFC 3339 with an offset. `Timestamp` deliberately has no timezone and uses
`YYYY-MM-DDTHH:MM:SS.sss`. Arrays are multi-valued Tantivy fields and validated arrays in SQLite.

```rust
use frankensteindb::{Analyzer, ColumnDef, ColumnType, Database, TableDef};

let mut database = Database::open("data")?;
database.create_table_def(TableDef {
    name: "products".into(),
    aliases: vec![],
    document_store: Default::default(),
    columns: vec![
        ColumnDef {
            name: "id".into(),
            data_type: ColumnType::Integer,
            primary_key: true,
            nullable: false,
            analyzer: None,
            compact_raw: false,
            index: Default::default(),
        },
        ColumnDef {
            name: "title".into(),
            data_type: ColumnType::Text,
            primary_key: false,
            nullable: false,
            analyzer: Some(Analyzer::Default),
            compact_raw: false,
            index: Default::default(),
        },
    ],
})?;
# Ok::<(), anyhow::Error>(())
```

## Reads

`Database::read(ReadRequest)` compiles `Filter` directly into Tantivy queries. It includes boosted
BM25, best-field disjunction-max scoring, fuzzy, single- and multi-word prefix/autocomplete, token
regex, positional regex phrases, JSON-path search/equality/ranges/existence, boolean composition, and
typed scalar filters. JSON path reads validate Tantivy's observed dynamic type before execution.

Large `in` filters compile to Tantivy `TermSetQuery`; values are deduplicated and do not create one
boolean clause each.

Structural filters compile through zero-valued `ConstScoreQuery` wrappers, so ranges, exact values,
null checks, cursor boundaries, and negation restrict results without changing text relevance.

Explicit native sorts return `next_search_after` for keyset pagination. Supplying it on the next
request compiles a lexicographic Tantivy range boundary and avoids deep `offset` collection. The
primary key is automatically appended as a stable tie-breaker.

An empty projection returns all table columns. Explicit projections may contain columns, `_score`,
or aggregate metrics. `Count`, `Sum`, `Average`, `Min`, and `Max` use Tantivy aggregation
collectors. `Database::explain(&request)` returns the chosen Tantivy collector without executing the
read. `Database::explain_score(&request, &identity_filter)` returns Tantivy's recursive BM25
explanation for one hit. HTTP exposes it at
`POST /api/v1/tables/{table}/rows/{key}/explain-score`. Highlight projections use Tantivy's actual
field tokenizer and `SnippetGenerator`, returning escaped HTML with matched tokens inside `<b>`.

```rust
use frankensteindb::{Filter, Projection, ReadRequest, Sort};

let result = database.read(ReadRequest {
    table: "products".into(),
    projection: vec![
        Projection::Column { column: "id".into(), alias: None },
        Projection::Column { column: "title".into(), alias: None },
        Projection::Score { alias: None },
    ],
    filter: Some(Filter::Search {
        fields: vec!["title".into()],
        query: "wireless headphones".into(),
    }),
    group_by: vec![],
    order_by: vec![Sort {
        column: "_score".into(),
        json_path: None,
        json_type: None,
        descending: true,
    }],
    limit: 20,
    offset: 0,
    search_after: None,
    min_score: None,
})?;
# Ok::<(), anyhow::Error>(())
```

## Writes and durability

`Mutation` has typed `Insert`, `Update`, and `Delete` variants. Update and delete filters are
resolved through Tantivy; SQLite receives only the matching primary keys and parameterized values.

- `mutate_typed` commits SQLite, publishes Tantivy, and reloads the reader.
- `mutate_typed_deferred` commits SQLite and stages Tantivy work until `flush`.
- `mutate_batch_typed` applies a vector atomically in SQLite, then publishes its outbox records.
- `bulk_insert_json_deferred` is the high-throughput schema-ordered ingestion path.

SQLite and its outbox commit in one transaction. If publishing Tantivy fails or the process stops,
the outbox is replayed on reopen. A read never falls back to SQLite.

Update/delete filters always use the currently published Tantivy snapshot. In an atomic batch,
all filter memberships are resolved before SQLite changes begin; an earlier item therefore cannot
make a row enter a later item's filter. The same rule applies while deferred changes are not yet
flushed.

## HTTP API

Start the JSON API:

```bash
cargo run --release --bin frankensteindb-server -- ./data --listen 127.0.0.1:8080
FRANKENSTEINDB_API_KEY=secret cargo run --release --bin frankensteindb-server -- ./data
```

Authentication is optional. A legacy full-access key or a JSON file containing hashed keys,
action scopes, validity windows, and table allowlists can be configured. There is no HTML UI,
login route, or cookie authentication. `GET /health`, `GET /metrics`, and `GET /openapi.json`
remain public.

Detailed examples are in [docs/API.md](docs/API.md). The OpenAPI 3.1 contract is in
[docs/openapi.json](docs/openapi.json) and is served by the process at `/openapi.json`.

Main endpoints:

- `GET /api/v1/tables`
- `POST /api/v1/tables` with a `TableDef`
- `GET` or `DELETE /api/v1/tables/{table}`
- `GET` or `POST /api/v1/tables/{table}/rows`
- `GET`, `PUT`, or `DELETE /api/v1/tables/{table}/rows/{key}`
- `PATCH` or `DELETE /api/v1/tables/{table}/rows` for filter-based mutations with optional
  `dry_run` and `max_rows`
- `PUT`, `PATCH`, or `DELETE /api/v1/tables/{table}/rows/{key}` with optional `If-Match`
- `POST /api/v1/tables/{table}/query` with a typed read body
- `POST /api/v1/tables/{table}/aggregate-intermediate` and `/aggregate-merge` for distributed
  aggregation
- `POST /api/v1/tables/{table}/explain`
- `POST /api/v1/tables/{table}/profile`
- `POST /api/v1/tables/{table}/rows/{key}/similar`
- `POST /api/v1/tables/{table}/facets/{column}`
- `PUT /api/v1/tables/{table}/aliases`
- `POST /api/v1/mutations` with an atomic mutation array
- `POST /api/v1/flush`
- `POST /api/v1/tables/{table}/reindex`
- `POST /api/v1/tables/{table}/optimize`
- `POST /api/v1/tables/{table}/imports` for streaming identity/gzip/zstd NDJSON upserts
- `POST /api/v1/tables/{table}/schema-changes`
- `GET /api/v1/jobs` and `GET/DELETE /api/v1/jobs/{id}`
- `POST /api/v1/backups` and offline `restore`

Add `?deferred=true` to row mutation endpoints to delay Tantivy visibility until flush.

```bash
curl -H 'Authorization: Bearer secret' \
  -H 'Content-Type: application/json' \
  -d '{
    "projection":[{"kind":"column","column":"id","alias":null}],
    "filter":{"kind":"search","fields":["title"],"query":"wireless"},
    "order_by":[],"limit":20,"offset":0
  }' \
  http://127.0.0.1:8080/api/v1/tables/products/query
```

## CLI

The default binary accepts typed JSON documents. Use `-` to read a document from standard input.

```bash
cargo run -- ./data tables
cargo run -- ./data create table.json
cargo run -- ./data read request.json
cargo run -- ./data mutate mutation.json --deferred
cargo run -- ./data flush
```

## Canonical VPM contracts

`canonical_contract_table()` returns the flattened 20-column schema. `canonical_contract_row()`
converts one canonical feed object into schema order. Documents are omitted. Primary and additional
suppliers become equivalent `tiekejuKodai` and `tiekejuPavadinimai` arrays; primary and additional
CPV codes become `bvpzKodai`.

## Benchmark

The benchmark uses only typed requests. It prints ingestion progress to stderr and the JSON report
to stdout.

```bash
cargo run --release --bin frankensteindb-benchmark -- \
  --reset --iterations 4 --import-threads 4 --index-threads 4 \
  --compression none

# Reuse the existing 5.8M-row database without importing again.
cargo run --release --bin frankensteindb-benchmark -- --reuse --iterations 4

# Also save readable query examples, representative results, and timing summaries.
cargo run --release --bin frankensteindb-benchmark -- --reuse --iterations 4 --save-results

# Or choose a different artifact path.
cargo run --release --bin frankensteindb-benchmark -- --reuse --save-results target/results.txt
```

`--save-results` without a value writes a human-readable Markdown report to `results.txt`. Each
benchmark section shows informational SQL-ish notation in a fenced block, an aligned timing table,
and up to ten representative result rows or aggregation items. Narrow results use a normal table;
wide results use a readable field/value table per row. The SQL is documentation only: benchmark
execution still uses typed requests and reads exclusively through Tantivy. The normal
machine-readable JSON summary remains on stdout.

The suite includes native reads and mutations plus recursive Tantivy aggregation cases: ordered
terms with missing values, bounded/keyed numeric and date histograms, keyed ranges, calendar
composites with missing ordering, stats/percentiles/cardinality, metric-ordered nested buckets,
filter buckets, top hits, and distributed intermediate collection/merge. JSON-path cases are not
run because the flattened canonical contract schema has no `Json` column.

New imports default to `--compression none` for maximum document-store write throughput. Use
`--compression lz4` or `--compression zstd --zstd-level 3` to measure size/CPU tradeoffs;
`--docstore-block-size` defaults to 16384. Compression only affects `stored` fields, so the canonical
benchmark schema—which currently projects from fast fields and has no stored columns—should show
little or no difference. With `--reuse`, these flags cannot rewrite the existing generation and the
report shows its persisted settings.

`--sqlite-synchronous full|normal|off`, writer memory, batch size, flush rows, and worker counts are
explicit benchmark controls. The benchmark is intentionally not run by repository verification.

To create a temporary 6 GB `tmpfs`, run the complete benchmark there, and unmount it afterward:

```bash
./scripts/benchmark-ramdisk.sh --iterations 2
```

The script requests `sudo` only for mounting and cleanup. Extra arguments are passed to the
benchmark, the dataset remains on disk, and the ramdisk is unmounted after success, failure, or
`Ctrl-C`. Override the mount point with `FRANKENSTEINDB_RAMDISK=/some/path`.

## Code layout

- `model.rs` — typed public schema, read, filter, projection, sort, and mutation contracts
- `database_*.rs` — public operations, recovery, Tantivy lifecycle, and SQLite storage boundaries
- `query/` — direct Tantivy filter compilation, projection, sorting, and aggregation
- `search_service.rs` — thread-safe, SQLite-free concurrent Tantivy reads
- `mutation/` — SQLite value conversion and parameterized storage writes
- `tantivy_schema.rs` — Tantivy schema and analyzer registration
- `bin/server/` — typed JSON REST API and optional Bearer authentication
- `bin/benchmark/` — canonical ingestion and typed performance cases

## License

Copyright © 2026 FrankensteinDB contributors.

FrankensteinDB is licensed under the GNU Affero General Public License version 3 only
(`AGPL-3.0-only`). See [LICENSE](LICENSE) for the complete terms.
