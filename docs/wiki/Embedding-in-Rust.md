# Embedding in Rust

FrankensteinDB can run behind its HTTP server or be embedded directly in a Rust process. The crate
name is `frankensteindb`.

## Open a database

```rust
use frankensteindb::Database;

let mut database = Database::open("./data")?;
# Ok::<(), anyhow::Error>(())
```

`open` creates missing directories, opens SQLite in WAL mode, loads table definitions and Tantivy
readers, and replays unfinished durable outbox records.

Only one writer process should own a database directory.

## Tune writer behavior

```rust
use frankensteindb::{Database, DatabaseOptions, SqliteSynchronous};

let database = Database::open_with_options("./data", DatabaseOptions {
    writer_memory_bytes: 512 * 1024 * 1024,
    writer_threads: 4,
    deleted_docs_merge_ratio: 0.2,
    min_merge_segments: 4,
    sqlite_wal_autocheckpoint_pages: 16_384,
    sqlite_synchronous: SqliteSynchronous::Full,
})?;
# Ok::<(), anyhow::Error>(())
```

`writer_memory_bytes` is per actively written table and must provide at least 15 MB per writer
thread. More threads help only when parsing/index construction is CPU-bound; storage or SQLite may
remain the bottleneck.

SQLite synchronization modes:

| Mode | Guarantee and use |
| --- | --- |
| `Full` | Default; sync every commit and protect acknowledged commits across sudden power loss |
| `Normal` | Keep database consistency, but a sudden power loss may lose recent commits |
| `Off` | Skip sync calls; only for disposable/rebuildable imports |

`deleted_docs_merge_ratio` must be in `(0, 1]`; `writer_threads`, `min_merge_segments`, and WAL
autocheckpoint pages must be positive.

## Define tables

The public schema types are `TableDef`, `ColumnDef`, `ColumnType`, `Analyzer`, `IndexProfile`,
`TextIndexRecord`, `DocumentStore`, and `DocumentCompression`.

```rust
use frankensteindb::{Analyzer, ColumnDef, ColumnType, Database, TableDef};

let mut database = Database::open("./data")?;
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

See [Schema and data types](Schema-and-data-types) for every type and analyzer.

## Write data

Object-shaped writes use `Mutation`:

```rust
use frankensteindb::Mutation;
use serde_json::json;
use std::collections::BTreeMap;

database.mutate_typed(Mutation::Insert {
    table: "products".into(),
    row: BTreeMap::from([
        ("id".into(), json!(1)),
        ("title".into(), json!("Wireless headphones")),
    ]),
})?;
# Ok::<(), anyhow::Error>(())
```

Available methods:

| Method | Use |
| --- | --- |
| `mutate_typed` | One immediately published insert/update/delete |
| `mutate_typed_deferred` | Durable write published by a later `flush` |
| `mutate_typed_limited` | Filter mutation guarded by an exact maximum row count |
| `mutate_batch_typed` | Atomic vector of typed mutations |
| `bulk_insert_json` | Schema-ordered rows, immediately published |
| `bulk_insert_json_deferred` | Schema-ordered inserts, deferred publish |
| `bulk_upsert_json_deferred` | Schema-ordered upserts, deferred publish |
| `validate_json_row` | Validate one schema-ordered row without writing |
| `flush` | Publish pending outbox operations and reload readers |

Schema-ordered bulk rows must contain exactly one value per column in table order.

## Read concurrently

`Database::read` works, but a cloned `SearchService` makes the read-only boundary explicit and can
be shared across threads or async tasks:

```rust
let search = database.search_service()?;
let another_reader = search.clone();
```

`SearchService` exposes table/catalog reads, typed reads, execution explanation, score explanation,
profile timings, recursive aggregations, distributed aggregation collect/merge, facets, and
more-like-this. Its reads never access SQLite.

The request/result types are `ReadRequest`, `Projection`, `Sort`, `Filter`, `Comparison`,
`QueryResult`, `Aggregation`, `Metric`, `CompositeSource`, and their supporting structs. See
[Complete feature and limit reference](Complete-feature-and-limit-reference).

After a schema/catalog change, the writer publishes updated handles to existing `SearchService`
clones.

## Administration

| Method | Purpose |
| --- | --- |
| `table`, `tables` | Read durable catalog definitions |
| `set_table_aliases` | Atomically replace aliases |
| `drop_table_named` | Drop table and derived index |
| `change_table_schema` | Shadow-table migration and generation swap |
| `reindex_table` | Rebuild Tantivy from SQLite |
| `optimize_table_with_options` | Concurrently merge balanced groups to a target segment count |
| `backup_to` | Create portable zstd backup |
| `restore_backup` | Offline verified restore |
| `ensure_row_etag` | Verify an optimistic-concurrency token inside the writer boundary |

## Canonical VPM contract helper

`canonical_contract_table()` returns the repository's flattened Lithuanian public-contract schema.
`canonical_contract_row(document)` converts one canonical feed object into schema order, combines
primary and additional suppliers into arrays, combines CPV codes into one array, and omits document
attachments.
