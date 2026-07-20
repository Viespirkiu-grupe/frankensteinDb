# Core concepts and guarantees

## One logical database, two storage engines

FrankensteinDB stores each logical row twice for different reasons:

- **SQLite is authoritative.** It owns transactions, canonical values, the schema catalog, jobs,
  audit records, and the durable indexing outbox.
- **Tantivy serves reads.** It owns searchable terms, fast fields, optional stored fields, scoring,
  sorting, and aggregations.

Public reads never quietly fall back to SQLite. This rule keeps performance predictable and makes
missing index capabilities visible instead of hiding them behind a slow path.

## Write lifecycle

A normal write follows this sequence:

1. Validate values against the table definition.
2. Commit the SQLite row change and an outbox record in one SQLite transaction.
3. Apply that outbox operation to the table's long-lived Tantivy writer.
4. Commit Tantivy and reload its reader.
5. Remove the completed outbox record and return.

If the process stops after step 2, FrankensteinDB replays the outbox when the database is reopened.
This prevents an acknowledged SQLite write from being permanently lost from the search index.

## What “durable” means here

- SQLite remains the recovery source of truth.
- Published reads use a consistent Tantivy reader snapshot.
- Normal writes become searchable before the request returns.
- Deferred writes are durable in SQLite but intentionally invisible to reads until `flush`.
- A complete Tantivy index can be rebuilt from SQLite with `reindex`.

SQLite and Tantivy do not participate in one distributed atomic commit. There can be a temporary
visibility gap after a crash or an indexing failure; durable outbox recovery closes that gap on
reopen. If your application requires a formally linearizable transaction spanning two independent
storage engines, FrankensteinDB does not claim that guarantee.

## Concurrency

- Tantivy reads can run concurrently and do not take the single writer lock.
- Writes are serialized through one database writer boundary.
- Each table keeps a long-lived Tantivy `IndexWriter`, allowing normal segment merge policy to work.
- Schema migration locks writes for that table and returns HTTP `423 Locked`; existing searchers
  can continue reading the old published generation.
- ETags provide optional optimistic concurrency for single-row `PUT`, `PATCH`, and `DELETE`.

## Filter-based writes

Updates and deletes first resolve their exact primary-key set through the currently published
Tantivy snapshot. SQLite then modifies only those keys. `max_rows` checks that same resolved set;
there is no separate count query that could race with the mutation.

For an atomic mutation batch, every filter membership is resolved before SQLite changes begin.
An earlier mutation in the batch cannot make a row enter a later mutation's filter.

## Deferred visibility

Add `?deferred=true` to supported row writes when importing or applying many changes:

```text
write 1 --+
write 2 --+--> SQLite + staged Tantivy operations --> POST /api/v1/flush --> visible
write 3 --+
```

Deferred mode reduces commit/reload overhead. During the interval, reads intentionally see the last
published snapshot. Call `POST /api/v1/flush` before depending on the new values.

## Files on disk

The database directory contains implementation-managed files similar to:

```text
data/
├── data.sqlite3
└── indexes/
    └── <table generation>/
```

Do not edit, copy individual files from, or run another SQLite client against a live directory.
Use the backup API for portable consistent copies.

## Current boundaries

- This is an MVP and its on-disk format does not yet promise perpetual compatibility.
- One server process should own a database directory.
- There is no SQL endpoint, SQL parser, browser UI, or cookie authentication.
- `search_after` is a live cursor, not a point-in-time snapshot.
- JSON-path sorting cannot currently use `search_after`.
- Documents stored in Tantivy are optional; normal projections use fast fields.
- Restores are offline operations and require the server to be stopped.
