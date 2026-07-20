# Search and queries

All supported reads execute through a published Tantivy snapshot. A query body chooses returned
columns, an optional filter, sorting, and pagination.

```json
{
  "projection": [
    {"kind":"column","column":"id","alias":null},
    {"kind":"column","column":"title","alias":null}
  ],
  "filter": null,
  "group_by": [],
  "order_by": [],
  "limit": 100,
  "offset": 0,
  "search_after": null,
  "min_score": null,
  "aggregations": {}
}
```

Send it to `POST /api/v1/tables/{table}/query`. Most fields have defaults and can be omitted.

## Projection

An empty `projection` returns every table column. Explicit projections are more efficient and make
API responses stable when the schema grows.

```json
[
  {"kind":"column","column":"id","alias":null},
  {"kind":"column","column":"title","alias":"name"},
  {"kind":"score","alias":"relevance"},
  {"kind":"highlight","column":"description","alias":"snippet","fragment_size":180}
]
```

Highlights are escaped HTML with matched tokens wrapped in `<b>`. They use Tantivy's
`SnippetGenerator` and the actual field analyzer. Highlighting requires suitable indexed term
information.

## Exact and structural filters

```json
{"kind":"compare","column":"price","operator":"greater_or_equal","value":10}
```

Comparison operators are `equal`, `not_equal`, `greater`, `greater_or_equal`, `less`, and
`less_or_equal`.

Other structural filters:

```json
{"kind":"between","column":"price","lower":10,"upper":100}
```

```json
{"kind":"in","column":"category","values":["audio","computers","phones"]}
```

```json
{"kind":"is_null","column":"retired_at","negated":false}
```

`in` uses Tantivy's `TermSetQuery`, so large lists do not become thousands of Boolean clauses.
Structural filters use constant scores and do not distort BM25 relevance.

## Combining filters

```json
{
  "kind":"all",
  "filters":[
    {"kind":"search","fields":["title"],"query":"wireless"},
    {"kind":"compare","column":"active","operator":"equal","value":true},
    {
      "kind":"not",
      "filter":{"kind":"in","column":"category","values":["archived"]}
    }
  ]
}
```

Use `all` for AND, `any` for OR, and `not` for negation. Empty Boolean groups are rejected.

## Full-text search

Basic multi-field BM25 search:

```json
{"kind":"search","fields":["title","description"],"query":"wireless headphones"}
```

The `query` uses Tantivy's natural query language, including terms, quoted phrases, Boolean
operators, and parentheses. When `fields` is empty, searchable text fields are selected by the query
parser.

Boost fields when a title match should matter more:

```json
{
  "kind":"search_boosted",
  "fields":{"title":4.0,"description":1.0},
  "query":"wireless headphones",
  "conjunction_by_default":false
}
```

Use best-field scoring when duplicate words in several fields should not simply add together:

```json
{
  "kind":"disjunction_max",
  "fields":{"title":4.0,"description":1.0},
  "query":"wireless headphones",
  "tie_breaker":0.1
}
```

The score is the best field score plus `tie_breaker` times the remaining field scores.

## Autocomplete, fuzzy, and regex

Single-token prefix:

```json
{"kind":"prefix","column":"title","value":"wire"}
```

Multi-token phrase-prefix:

```json
{
  "kind":"phrase_prefix",
  "column":"title",
  "phrase":"public proc",
  "max_expansions":50
}
```

The final analyzed token acts as the prefix. Phrase-prefix requires `record: positions`.

Typo-tolerant single term:

```json
{
  "kind":"fuzzy",
  "column":"title",
  "value":"headphnes",
  "distance":1,
  "transposition_cost_one":true
}
```

Regex against a complete indexed term:

```json
{"kind":"regex","column":"title","pattern":"head.*"}
```

Regex does not search the original un-tokenized string unless the field uses `Raw`. A leading `.*`
can scan much of the term dictionary and should be used carefully.

Regex phrase against consecutive token positions:

```json
{
  "kind":"regex_phrase",
  "column":"title",
  "patterns":["wire.*","head.*"],
  "slop":0,
  "max_expansions":4096
}
```

This requires `record: positions`. Patterns target indexed terms and are not passed through the
analyzer again.

## JSON paths

JSON columns support typed dynamic paths without reading SQLite:

```json
{
  "kind":"json_between",
  "column":"metadata",
  "path":"pricing.amount",
  "data_type":"f64",
  "lower":10,
  "upper":100
}
```

Available operations are `json_search`, `json_compare`, `json_between`, and `json_exists`. Typed
paths use `string`, `i64`, `u64`, `f64`, `bool`, or `date_time`. FrankensteinDB checks Tantivy's
observed dynamic type and rejects mismatches rather than silently coercing them.

JSON sorting identifies both path and type:

```json
{"column":"metadata","json_path":"rank","json_type":"i64","descending":false}
```

JSON-path sorting currently cannot use `search_after`.

## Sorting and pagination

```json
"order_by":[{"column":"created_at","descending":true}]
```

You can sort by native scalar columns, `_score`, projection aliases, and typed JSON paths. Deep
pages should use `search_after` instead of increasing `offset`.

The first page needs an explicit non-nullable scalar sort. If more results exist, the response
contains:

```json
{"meta":{"next_search_after":["2026-07-20T10:00:00Z",42]}}
```

Copy that array unchanged into the next request:

```json
{
  "order_by":[{"column":"created_at","descending":true}],
  "search_after":["2026-07-20T10:00:00Z",42],
  "limit":100
}
```

FrankensteinDB appends the primary key as an ascending tie-breaker. Keep the same filter and sort
between pages. The cursor is live rather than a point-in-time snapshot, so concurrent writes can
affect later pages.

## Diagnostics

- `POST .../explain` returns the execution plan without running the read.
- `POST .../profile` returns compilation, collection, materialization, segment, and total timing.
- `POST .../rows/{key}/explain-score` explains one matching document's BM25 score recursively.

The score explanation shows boosts, term frequency, inverse document frequency, field-length
normalization, and the final score.

## More like this

```bash
curl -X POST "$FDB/api/v1/tables/products/rows/42/similar" \
  -H "$AUTH" -H 'Content-Type: application/json' \
  -d '{"fields":["title","description"],"max_query_terms":25,"limit":20}'
```

The selected row provides seed terms. You can tune document/term frequencies, word lengths, stop
words, boost factor, result limit, and minimum score. The seed row is excluded from results.
