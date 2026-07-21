# Complete feature and limit reference

This page is the exhaustive checklist. The task-oriented pages explain how to use each feature;
this page makes it easy to verify whether a specific public type or operation is supported.

## Column types

All public column types:

```text
Integer       Unsigned       Real          Text
Boolean       Date           DateTime      Timestamp
Blob          Ip             Json          Facet
TextArray     IntegerArray   UnsignedArray RealArray
BooleanArray  DateArray      DateTimeArray TimestampArray
BlobArray     IpArray        JsonArray     FacetArray
GeoPoint      GeoPointArray
```

Primary keys are exactly one non-nullable `Integer` or `Text` column. Names beginning with `__aq_`
and the virtual column name `_score` are reserved. Names are unique case-insensitively. Facet
columns must remain indexed.
Geo columns must remain indexed. `GeoPoint` is `{lat,lon}` and `GeoPointArray` is an array of those
objects; coordinates are finite WGS84 degrees and arrays contain at most 10,000 points.

## Analyzer variants and validation

| Variant | Parameters and limits |
| --- | --- |
| `Default` | General-purpose simple tokenizer, lowercase, long-token removal |
| `Raw` | Complete input is one exact term |
| `Whitespace` | Whitespace tokenizer |
| `Stem(language)` | One supported built-in language name |
| `Ngram` | `1 <= min <= max <= 32`; `prefix_only` is Boolean |
| `Custom` | Optional stemmer, ASCII folding, stop words, and synonyms |

`Custom` allows at most 10,000 non-empty stop words and 10,000 synonym keys. Synonym keys must be
lowercase and non-empty; every key needs at least one non-empty expansion.

Built-in stemming languages: Arabic, Danish, Dutch, English, Finnish, French, German, Greek,
Hungarian, Italian, Norwegian, Portuguese, Romanian, Russian, Spanish, Swedish, Tamil, and Turkish.

Text and text-array columns require an analyzer. Other types reject one.

## Index and document-store options

| Model | All fields |
| --- | --- |
| `IndexProfile` | `indexed`, `record`, `stored` |
| `TextIndexRecord` | `basic`, `frequencies`, `positions` |
| `DocumentStore` | `compression`, `zstd_level`, `block_size`, `dedicated_thread` |
| `DocumentCompression` | `lz4`, `zstd`, `none` |

Document-store block size is 1 KiB through 16 MiB. Defaults are LZ4, 16 KiB blocks, no explicit
Zstd level (Tantivy uses level 3), and a dedicated compression thread.

## Projection variants

| `kind` | Fields |
| --- | --- |
| `column` | `column`, optional `alias` |
| `score` | optional `alias` |
| `highlight` | `column`, optional `alias`, `fragment_size` 32–4,096 |
| `aggregate` | `function`, optional `column`, required `alias` |
| `geo_distance` | `column`, `from`, `mode` (`min`, `max`, `average`), optional `alias` |

Simple projection aggregates are `count`, `sum`, `average`, `min`, and `max`. Recursive aggregation
requests have the larger metric set listed below.

## Filter variants

| `kind` | Main fields | Important limits |
| --- | --- | --- |
| `compare` | `column`, `operator`, `value` | Value must match column type |
| `between` | `column`, `lower`, `upper` | Both bounds inclusive |
| `in` | `column`, `values` | At least one value; deduplicating TermSet query |
| `is_null` | `column`, `negated` | Checks presence fast field |
| `search` | `fields`, `query` | Non-empty query; empty fields selects searchable text |
| `search_boosted` | `fields`, `query`, `conjunction_by_default` | At least one positive finite boost |
| `fuzzy` | `column`, `value`, `distance`, `transposition_cost_one` | One analyzed token; distance 0–2 |
| `prefix` | `column`, `value` | One analyzed token |
| `phrase_prefix` | `column`, `phrase`, `max_expansions` | 2–32 tokens; 1–4,096 expansions; positions required |
| `disjunction_max` | `fields`, `query`, `tie_breaker` | 1–32 fields; tie breaker 0–1 |
| `regex` | `column`, `pattern` | Pattern 1–512 bytes |
| `regex_phrase` | `column`, `patterns`, `slop`, `max_expansions` | 2–16 patterns; slop ≤64; positions required |
| `json_search` | `column`, `path`, `query` | Indexed `Json` or `JsonArray` |
| `json_compare` | `column`, `path`, `data_type`, `operator`, `value` | Boolean paths allow only equal/not-equal |
| `json_between` | `column`, `path`, `data_type`, `lower`, `upper` | Inclusive typed bounds |
| `json_exists` | `column`, `path`, optional `data_type`, `negated` | Type may be omitted for any scalar |
| `geo_distance` | `column`, `center`, `radius_meters` | Exact Haversine radius; radius finite and non-negative |
| `geo_bounding_box` | `column`, `bounds` | Exact rectangle; west > east crosses antimeridian |
| `geo_distance_compare` | column, center, mode, operator, distance | Cursor-oriented reduced distance comparison |
| `all` | `filters` | At least one child; logical AND |
| `any` | `filters` | At least one child; logical OR |
| `not` | `filter` | One child |

Regex-phrase patterns are each 1–512 bytes. `max_expansions` is 1–16,384. Phrase-prefix input is at
most 1,024 bytes. Structural filters are constant-score filters.

Comparison operators: `equal`, `not_equal`, `greater`, `greater_or_equal`, `less`, and
`less_or_equal`.

JSON path types: `string`, `i64`, `u64`, `f64`, `bool`, and `date_time`. `date_time` values are RFC
3339 strings. Typed operations require exactly one observed dynamic Tantivy type at that path.

## Sorting and paging

`Sort` fields are `column`, optional `json_path`, optional `json_type`, `descending`, optional
`geo_distance_from`, and `geo_distance_mode` (`min`, `max`, or `average`).
`ReadBody` fields are `projection`, `filter`, `group_by`, `order_by`, `limit`, `offset`,
`search_after`, `min_score`, and `aggregations`.

- Typed query limit is capped at 10,000; default is 100.
- Simple `GET /rows` limit is clamped to 1–1,000.
- `search_after` requires an explicit indexed non-nullable native scalar sort.
- Cursor values must include the automatically appended primary-key tie-breaker.
- Cursor paging cannot combine with offset, aggregations, score sorting/projection, `min_score`,
  nullable/array sorting, or JSON-path sorting.
- Geo-distance sorting is supported for scalar and array geo columns; a null/empty geo value cannot
  be the cursor boundary.

## Recursive aggregation variants

| `kind` | Main parameters |
| --- | --- |
| `terms` | column, size, segment size, minimum count, missing, order, children |
| `histogram` | column, interval, offset, minimum count, hard/extended bounds, keyed, children |
| `date_histogram` | column, fixed interval, offset, minimum count, bounds, keyed, children |
| `range` | column, ranges, keyed, children |
| `json_terms` | typed JSON target, size, missing, order, children |
| `json_histogram` | numeric JSON target, interval, count, bounds, keyed, children |
| `json_range` | numeric JSON target, ranges, keyed, children |
| `filter` | filter and children |
| `composite` | sources, size, after cursor, children |
| `metric` | function, column or JSON path, percentiles, missing |
| `top_hits` | size, sort, columns |
| `geo_tile_grid` | geo column, zoom 0–31, max buckets, document/point count mode, optional bounds |

Limits:

- At most 100 top-level aggregations and four recursive levels.
- Terms and composite size: 1–10,000.
- Terms `segment_size`: at least `size` and at most 100,000.
- Histogram intervals must be positive and finite.
- Range aggregation: 1–1,000 buckets; keyed ranges require every key.
- Composite: 1–16 named sources.
- `top_hits`: child aggregation only, size 1–100.
- `geo_tile_grid`: top-level/local only, 1–100,000 buckets; excluded from distributed payloads.
- Distributed merge: 1–1,024 payloads and at most 256 MiB decoded payload data.

Metric functions: `count`, `sum`, `average`, `min`, `max`, `cardinality`, `percentiles`, `stats`, and
`extended_stats`. A metric accepts a native `column` or a typed `json_path`, never both. Non-count
numeric JSON metrics require `i64`, `u64`, or `f64`.

Composite source variants:

| `kind` | Fields |
| --- | --- |
| `terms` | name, column, descending, missing bucket/order |
| `histogram` | name, column, positive interval, descending, missing bucket/order |
| `date_histogram` | name, column, exactly one fixed/calendar interval, ordering/missing options |
| `json_terms` | name, typed JSON target, ordering/missing options |

Calendar intervals are `year`, `month`, and `week`. Missing order is `default`, `first`, or `last`.

## More-like-this options

| Field | Default or limit |
| --- | --- |
| `fields` | Empty selects all indexed text/text-array fields |
| `filter` | Optional constant-score restriction |
| `min_doc_frequency` | 2 |
| `max_doc_frequency` | Unlimited when omitted |
| `min_term_frequency` | 1 |
| `max_query_terms` | 25; allowed 1–1,024 |
| `min_word_length`, `max_word_length` | Optional |
| `boost_factor` | 1; positive and finite |
| `stop_words` | Empty |
| `limit` | 20; allowed 1–1,000 |
| `min_score` | Optional finite score |

## Mutation and schema variants

Mutation kinds are `insert`, `update`, and `delete`. Schema-change kinds are `add_column`,
`drop_column`, `rename_column`, `alter_column`, and `alter_document_store`.

The operational job kinds are import, schema change, reindex, optimize, and backup. Table aliases,
facet collection, score explanation, query profiling, ETags, deferred visibility, audit, and auth
reload are described in the task-oriented pages and listed in [HTTP API reference](HTTP-API-reference).
