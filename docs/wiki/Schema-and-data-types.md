# Schema and data types

A table definition controls validation, Tantivy indexing, and how values are returned. Schema
choices are important: changing them later starts a background rebuild.

## Table definition

```json
{
  "name": "events",
  "aliases": ["current_events"],
  "document_store": {
    "compression": "lz4",
    "zstd_level": null,
    "block_size": 16384,
    "dedicated_thread": true
  },
  "columns": [
    {
      "name": "id",
      "data_type": "Integer",
      "primary_key": true,
      "nullable": false,
      "analyzer": null,
      "compact_raw": false,
      "index": {"indexed": true, "record": "positions", "stored": false}
    }
  ]
}
```

Rules:

- A table has exactly one primary key.
- The primary key is a non-nullable `Integer` or `Text` column.
- Column and table names are validated before any SQLite object is created.
- A nullable column accepts JSON `null`; a non-nullable column does not.
- Arrays contain values, not nullable elements.
- Aliases are alternate names for the same table and published index generation.

## Scalar types

| Type | JSON input | Notes |
| --- | --- | --- |
| `Integer` | `-42` | Signed 64-bit integer |
| `Unsigned` | `42` | Unsigned 64-bit integer |
| `Real` | `12.5` | Finite 64-bit floating-point number |
| `Text` | `"hello"` | Requires an analyzer |
| `Boolean` | `true` | `true` or `false`, not `0` or `1` |
| `Date` | `"2026-07-21"` | Calendar date, format `YYYY-MM-DD` |
| `DateTime` | `"2026-07-21T13:30:00+03:00"` | RFC 3339 instant with an offset |
| `Timestamp` | `"2026-07-21T13:30:00.123"` | Deliberately timezone-less |
| `Blob` | `"0x89504e47"` | Hexadecimal bytes; `0x` is optional |
| `Ip` | `"192.0.2.10"` | IPv4 or IPv6 |
| `Json` | `{"source":"web"}` | Must be a JSON object |
| `Facet` | `"/catalog/audio/headphones"` | Hierarchical path beginning with `/` |

`DateTime` and `Timestamp` are intentionally different. Use `DateTime` when the value identifies an
instant globally. Use `Timestamp` when the source data has no timezone and adding one would change
its meaning.

## Array types

Every scalar family that Tantivy can hold as repeated values has a corresponding array type:

| Array type | Example |
| --- | --- |
| `TextArray` | `["wireless", "sale"]` |
| `IntegerArray` | `[-2, 8]` |
| `UnsignedArray` | `[2, 8]` |
| `RealArray` | `[1.5, 8.0]` |
| `BooleanArray` | `[true, false]` |
| `DateArray` | `["2026-07-20", "2026-07-21"]` |
| `DateTimeArray` | `["2026-07-21T10:00:00Z"]` |
| `TimestampArray` | `["2026-07-21T10:00:00.000"]` |
| `BlobArray` | `["0x00ff", "cafe"]` |
| `IpArray` | `["192.0.2.1", "2001:db8::1"]` |
| `JsonArray` | `[{"sku":"A"}, {"sku":"B"}]` |
| `FacetArray` | `["/a/b", "/a/c"]` |

An exact or range filter on an array matches a row when any array item matches. The canonical array
is returned as an array; it is not flattened into duplicate rows.

## Text analyzers

An analyzer controls how text becomes indexed terms. The same analyzer is used at query time where
appropriate.

| Analyzer | Good for | Behavior |
| --- | --- | --- |
| `"Default"` | Natural language | General-purpose tokenization and lowercasing |
| `"Raw"` | Codes, IDs, exact strings | Keeps the complete value as one term |
| `"Whitespace"` | Pre-tokenized text | Splits only on whitespace |
| `{"Stem":"english"}` | Language search | Lowercases and stems one of Tantivy's supported languages |
| `{"Ngram":{...}}` | Autocomplete | Emits character n-grams |
| `{"Custom":{...}}` | Controlled language search | Optional stemmer, stop words, synonyms, and ASCII folding |

Examples:

```json
{"Stem":"english"}
```

```json
{
  "Custom": {
    "stem": "english",
    "stop_words": ["the", "and"],
    "synonyms": {"tv": ["television"]},
    "ascii_folding": true
  }
}
```

Use `Raw` for organization codes, invoice numbers, and other values that should not be split. Use a
language analyzer for prose. Synonyms are one-token expansions at the same token position.

Built-in stemming names are `arabic`, `danish`, `dutch`, `english`, `finnish`, `french`, `german`,
`greek`, `hungarian`, `italian`, `norwegian`, `portuguese`, `romanian`, `russian`, `spanish`,
`swedish`, `tamil`, and `turkish`. Unsupported names are rejected when the schema is validated.

## Index profile

Every column accepts:

```json
{"indexed":true,"record":"positions","stored":false}
```

| Setting | Meaning |
| --- | --- |
| `indexed` | Makes the column queryable. Canonical fast-field projection remains available. |
| `record: basic` | Terms only; smallest option for exact/filter usage |
| `record: frequencies` | Adds frequencies and supports BM25 scoring |
| `record: positions` | Adds positions and supports phrase queries and tokenizer-aware snippets |
| `stored` | Also writes the value to Tantivy's document store |

The defaults are `indexed: true`, `record: positions`, and `stored: false`. `record` matters only for
text fields. Keep `stored: false` unless a feature specifically needs the document store; normal
result projection reads Tantivy fast fields.

## Document-store compression

The table-level `document_store` applies only to columns with `index.stored: true`:

| Compression | Tradeoff |
| --- | --- |
| `lz4` | Default; fast and moderately compact |
| `zstd` | Smaller data at higher CPU cost; default level is 3 |
| `none` | Fastest writing, largest stored-document blocks |

`block_size` defaults to 16 KiB. `dedicated_thread` defaults to `true`. Changing these settings on an
existing table requires an `alter_document_store` schema change and rebuilds the Tantivy generation.

## Schema changes

Send an array to `POST /api/v1/tables/{table}/schema-changes`:

```json
[
  {
    "kind":"add_column",
    "column":{
      "name":"stock", "data_type":"Integer",
      "primary_key":false, "nullable":false, "analyzer":null
    },
    "default":0
  },
  {"kind":"rename_column","from":"title","to":"name"}
]
```

Supported changes are `add_column`, `drop_column`, `rename_column`, `alter_column`, and
`alter_document_store`. Primary-key removal, type changes, and renames are restricted where they
would break row identity. FrankensteinDB validates and converts every row into a shadow table,
builds a complete Tantivy generation, then swaps it into place.
