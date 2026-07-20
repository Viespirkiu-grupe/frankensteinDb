# Aggregations and analysis

FrankensteinDB exposes Tantivy's aggregation collector through the normal query endpoint. Add a
named `aggregations` object to a query body. The name becomes the corresponding response key.

## Terms: top values

```json
{
  "limit":0,
  "aggregations":{
    "top_categories":{
      "kind":"terms",
      "column":"category",
      "size":20,
      "segment_size":100,
      "min_doc_count":1,
      "missing":"unknown",
      "order":{"target":"_count","descending":true},
      "aggregations":{}
    }
  }
}
```

`order.target` may be `_count`, `_key`, or the name of a compatible child metric. A larger
`segment_size` can improve accuracy for globally rare terms at additional CPU and memory cost.

## Numeric histogram

```json
{
  "kind":"histogram",
  "column":"price",
  "interval":50,
  "offset":0,
  "min_doc_count":0,
  "hard_bounds":{"min":0,"max":1000},
  "extended_bounds":{"min":0,"max":1000},
  "keyed":true,
  "aggregations":{}
}
```

- `hard_bounds` prevents buckets outside the boundary.
- `extended_bounds` asks for empty boundary buckets too.
- `keyed` returns an object keyed by bucket rather than an array.

## Date histogram

```json
{
  "kind":"date_histogram",
  "column":"created_at",
  "fixed_interval":"1d",
  "offset":"0h",
  "min_doc_count":0,
  "hard_bounds":null,
  "extended_bounds":null,
  "keyed":false,
  "aggregations":{}
}
```

This aggregation uses fixed durations. Calendar-aware `year`, `month`, and `week` intervals are
available as composite date-histogram sources.

## Explicit ranges

```json
{
  "kind":"range",
  "column":"price",
  "keyed":true,
  "ranges":[
    {"key":"budget","from":null,"to":50},
    {"key":"standard","from":50,"to":200},
    {"key":"premium","from":200,"to":null}
  ],
  "aggregations":{}
}
```

Lower bounds are inclusive; upper bounds are exclusive. A key is required for every range when
`keyed` is true.

## Metrics

```json
{
  "kind":"metric",
  "function":"stats",
  "column":"price",
  "json_path":null,
  "percents":null,
  "missing":null
}
```

Available metric functions:

| Function | Result |
| --- | --- |
| `count` | Number of non-missing values |
| `sum` | Sum |
| `average` | Arithmetic mean |
| `min`, `max` | Extremes |
| `cardinality` | Approximate distinct count |
| `percentiles` | Requested percentile values |
| `stats` | Count, min, max, average, and sum |
| `extended_stats` | Stats plus variance and standard deviation |

For percentiles, provide values such as `"percents":[50,95,99]`.

## Nested aggregations

Each bucket aggregation accepts child `aggregations`. This example orders categories by their
average price:

```json
{
  "kind":"terms",
  "column":"category",
  "size":10,
  "order":{"target":"average_price","descending":true},
  "aggregations":{
    "average_price":{
      "kind":"metric",
      "function":"average",
      "column":"price",
      "json_path":null,
      "percents":null,
      "missing":null
    }
  }
}
```

Aggregation nesting is limited to four levels and the request may contain at most 100 top-level
aggregations.

## Filter buckets

```json
{
  "kind":"filter",
  "filter":{"kind":"compare","column":"active","operator":"equal","value":true},
  "aggregations":{
    "prices":{"kind":"metric","function":"stats","column":"price","json_path":null}
  }
}
```

Filter buckets are useful when several metrics should share one additional predicate.

## Top hits inside buckets

```json
{
  "kind":"top_hits",
  "size":3,
  "sort":[{"column":"created_at","descending":true}],
  "columns":["id","title","created_at"]
}
```

Use `top_hits` as a child aggregation to return representative documents per bucket.

## Composite buckets and pagination

Composite aggregations produce stable multi-source bucket keys and an `after_key` for the next
page:

```json
{
  "kind":"composite",
  "size":100,
  "sources":[
    {
      "kind":"date_histogram",
      "name":"month",
      "column":"created_at",
      "fixed_interval":null,
      "calendar_interval":"month",
      "descending":true,
      "missing_bucket":true,
      "missing_order":"last"
    },
    {
      "kind":"terms",
      "name":"category",
      "column":"category",
      "descending":false,
      "missing_bucket":false,
      "missing_order":"default"
    }
  ],
  "after":{},
  "aggregations":{}
}
```

Copy the returned `after_key` object into `after` to request the next bucket page. Calendar
intervals are `year`, `month`, and `week`; they are aligned to UTC.

## JSON-path aggregations

JSON objects and object arrays support `json_terms`, `json_histogram`, and `json_range`. Point a
request at a typed path:

```json
{
  "kind":"json_terms",
  "target":{"column":"metadata","path":"supplier.country","data_type":"string"},
  "size":20,
  "missing":"unknown",
  "order":{"target":"_count","descending":true},
  "aggregations":{}
}
```

Metric aggregations can use `json_path` instead of `column`. FrankensteinDB verifies the observed
dynamic Tantivy type before collecting.

## Distributed aggregation

For application-managed shards:

1. Send the same aggregation tree to `POST .../aggregate-intermediate` on each shard.
2. Collect each opaque hexadecimal payload.
3. Send the unchanged tree and payload list to `POST .../aggregate-merge`.

The intermediate payload is versioned and bound to a hash of the request. Merge rejects mismatched
requests, unknown versions, more than 1,024 payloads, or more than 256 MiB of decoded intermediate
data. Treat payloads as opaque; do not parse or persist them as a long-term format.
