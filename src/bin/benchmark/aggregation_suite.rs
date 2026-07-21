use std::collections::BTreeMap;

use frankensteindb::{
    Aggregation, AggregationRange, BucketOrder, CalendarInterval, Comparison, CompositeSource,
    Filter, GeoDistanceMode, HistogramBounds, Metric, MissingOrder, SearchService, Sort,
};
use serde_json::{Value, json};

use super::*;

const AGGREGATION_BENCHMARK_COUNT: usize = 13;

pub(crate) const fn aggregation_benchmark_count() -> usize {
    AGGREGATION_BENCHMARK_COUNT
}

pub(crate) fn run_aggregation_benchmarks(
    database: &Database,
    iterations: usize,
    search_threads: usize,
    progress: &ProgressReporter,
    first_index: usize,
    total: usize,
    mut capture: Option<&mut BenchmarkCapture>,
) -> Result<Vec<Measurement>> {
    let uncached_search = database.search_service_with_options(SearchOptions {
        worker_threads: search_threads,
        aggregation_cache_entries: 0,
        warmup_fast_fields: false,
    })?;
    let cases = aggregation_cases();
    debug_assert_eq!(cases.len() + 4, AGGREGATION_BENCHMARK_COUNT);
    let mut measurements = Vec::with_capacity(AGGREGATION_BENCHMARK_COUNT);
    for (offset, case) in cases.into_iter().enumerate() {
        progress.benchmark_start(first_index + offset, total, case.name, iterations);
        let measurement =
            measure_aggregation(&uncached_search, &case, iterations, capture.as_deref_mut())?;
        progress.benchmark_done(&measurement);
        measurements.push(measurement);
    }

    let facet_bundle = AggregationCase {
        name: "facet_bundle_uncached",
        filter: None,
        request: facet_bundle(),
    };
    let index = first_index + measurements.len();
    progress.benchmark_start(index, total, facet_bundle.name, iterations);
    let measurement = measure_aggregation(
        &uncached_search,
        &facet_bundle,
        iterations,
        capture.as_deref_mut(),
    )?;
    progress.benchmark_done(&measurement);
    measurements.push(measurement);

    let cached_search = database.search_service_with_options(SearchOptions {
        worker_threads: search_threads,
        aggregation_cache_entries: 8,
        warmup_fast_fields: false,
    })?;
    let cached_bundle = AggregationCase {
        name: "facet_bundle_cached",
        filter: None,
        request: facet_bundle.request.clone(),
    };
    let index = first_index + measurements.len();
    progress.benchmark_start(index, total, cached_bundle.name, iterations);
    let measurement = measure_aggregation(
        &cached_search,
        &cached_bundle,
        iterations,
        capture.as_deref_mut(),
    )?;
    progress.benchmark_done(&measurement);
    measurements.push(measurement);

    let distributed = distributed_request();
    let index = first_index + measurements.len();
    progress.benchmark_start(index, total, "distributed_aggregation_collect", iterations);
    let measurement = measure_distributed_collect(
        &uncached_search,
        &distributed,
        iterations,
        capture.as_deref_mut(),
    )?;
    progress.benchmark_done(&measurement);
    measurements.push(measurement);

    progress.benchmark_start(
        index + 1,
        total,
        "distributed_aggregation_merge",
        iterations,
    );
    let measurement =
        measure_distributed_merge(&uncached_search, &distributed, iterations, capture)?;
    progress.benchmark_done(&measurement);
    measurements.push(measurement);
    Ok(measurements)
}

struct AggregationCase {
    name: &'static str,
    filter: Option<Filter>,
    request: BTreeMap<String, Aggregation>,
}

fn aggregation_cases() -> Vec<AggregationCase> {
    vec![
        case("terms_order_missing", terms_order_missing()),
        case("bounded_keyed_histogram", bounded_keyed_histogram()),
        case("keyed_numeric_ranges", keyed_numeric_ranges()),
        case("bounded_keyed_date_histogram", bounded_date_histogram()),
        case("calendar_composite_missing_order", calendar_composite()),
        case("advanced_numeric_metrics", advanced_metrics()),
        case("metric_ordered_terms", metric_ordered_terms()),
        AggregationCase {
            name: "filtered_bucket_aggregation",
            filter: Some(Filter::Compare {
                column: "istrinta".into(),
                operator: Comparison::Equal,
                value: json!(false),
            }),
            request: filtered_bucket(),
        },
        case("top_hits_per_type", top_hits_per_type()),
    ]
}

fn case(name: &'static str, request: BTreeMap<String, Aggregation>) -> AggregationCase {
    AggregationCase {
        name,
        filter: None,
        request,
    }
}

fn terms_order_missing() -> BTreeMap<String, Aggregation> {
    BTreeMap::from([(
        "categories".into(),
        Aggregation::Terms {
            column: "kategorija".into(),
            size: 20,
            segment_size: Some(100),
            min_doc_count: Some(1),
            missing: Some(json!("__missing__")),
            order: Some(BucketOrder {
                target: "_count".into(),
                descending: true,
            }),
            aggregations: BTreeMap::new(),
        },
    )])
}

fn facet_bundle() -> BTreeMap<String, Aggregation> {
    let terms = |column: &str, size| Aggregation::Terms {
        column: column.into(),
        size,
        segment_size: Some(size.saturating_mul(2)),
        min_doc_count: Some(1),
        missing: None,
        order: Some(BucketOrder {
            target: "_count".into(),
            descending: true,
        }),
        aggregations: BTreeMap::new(),
    };
    BTreeMap::from([
        ("types".into(), terms("tipas", 50)),
        ("categories".into(), terms("kategorija", 50)),
        (
            "buyers".into(),
            terms("perkanciosiosOrganizacijosKodas", 50),
        ),
        ("suppliers".into(), terms("tiekejuKodai", 50)),
        ("cpv".into(), terms("bvpzKodai", 50)),
        (
            "value_ranges".into(),
            Aggregation::Range {
                column: "numatomaVerte".into(),
                ranges: vec![
                    AggregationRange {
                        key: Some("small".into()),
                        from: None,
                        to: Some(json!(10_000)),
                    },
                    AggregationRange {
                        key: Some("medium".into()),
                        from: Some(json!(10_000)),
                        to: Some(json!(100_000)),
                    },
                    AggregationRange {
                        key: Some("large".into()),
                        from: Some(json!(100_000)),
                        to: None,
                    },
                ],
                keyed: true,
                aggregations: BTreeMap::new(),
            },
        ),
        (
            "contract_years".into(),
            Aggregation::DateHistogram {
                column: "sudarymoData".into(),
                fixed_interval: "365d".into(),
                offset: None,
                min_doc_count: 1,
                hard_bounds: None,
                extended_bounds: None,
                keyed: false,
                aggregations: BTreeMap::new(),
            },
        ),
    ])
}

fn bounded_keyed_histogram() -> BTreeMap<String, Aggregation> {
    let bounds = HistogramBounds {
        min: json!(0),
        max: json!(10_000_000),
    };
    BTreeMap::from([(
        "values".into(),
        Aggregation::Histogram {
            column: "numatomaVerte".into(),
            interval: 250_000.0,
            offset: Some(0.0),
            min_doc_count: 0,
            hard_bounds: Some(bounds.clone()),
            extended_bounds: Some(bounds),
            keyed: true,
            aggregations: BTreeMap::new(),
        },
    )])
}

fn keyed_numeric_ranges() -> BTreeMap<String, Aggregation> {
    let range = |key: &str, from: Option<Value>, to: Option<Value>| AggregationRange {
        key: Some(key.into()),
        from,
        to,
    };
    BTreeMap::from([(
        "value_bands".into(),
        Aggregation::Range {
            column: "numatomaVerte".into(),
            ranges: vec![
                range("small", None, Some(json!(10_000))),
                range("medium", Some(json!(10_000)), Some(json!(100_000))),
                range("large", Some(json!(100_000)), None),
            ],
            keyed: true,
            aggregations: BTreeMap::new(),
        },
    )])
}

fn bounded_date_histogram() -> BTreeMap<String, Aggregation> {
    let bounds = HistogramBounds {
        min: json!("2000-01-01"),
        max: json!("2030-12-31"),
    };
    BTreeMap::from([(
        "contract_years".into(),
        Aggregation::DateHistogram {
            column: "sudarymoData".into(),
            fixed_interval: "365d".into(),
            offset: None,
            min_doc_count: 0,
            hard_bounds: Some(bounds.clone()),
            extended_bounds: Some(bounds),
            keyed: true,
            aggregations: BTreeMap::new(),
        },
    )])
}

fn calendar_composite() -> BTreeMap<String, Aggregation> {
    BTreeMap::from([(
        "publication_months".into(),
        Aggregation::Composite {
            sources: vec![CompositeSource::DateHistogram {
                name: "month".into(),
                column: "paskelbimoData".into(),
                fixed_interval: None,
                calendar_interval: Some(CalendarInterval::Month),
                descending: true,
                missing_bucket: true,
                missing_order: MissingOrder::Last,
            }],
            size: 100,
            after: BTreeMap::new(),
            aggregations: BTreeMap::new(),
        },
    )])
}

fn advanced_metrics() -> BTreeMap<String, Aggregation> {
    BTreeMap::from([
        ("stats".into(), metric(Metric::Stats, "numatomaVerte")),
        (
            "extended_stats".into(),
            metric(Metric::ExtendedStats, "numatomaVerte"),
        ),
        (
            "organizations".into(),
            metric(Metric::Cardinality, "perkanciosiosOrganizacijosKodas"),
        ),
        (
            "percentiles".into(),
            Aggregation::Metric {
                function: Metric::Percentiles,
                column: Some("numatomaVerte".into()),
                json_path: None,
                percents: Some(vec![50.0, 95.0, 99.0]),
                missing: Some(json!(0)),
            },
        ),
    ])
}

fn metric(function: Metric, column: &str) -> Aggregation {
    Aggregation::Metric {
        function,
        column: Some(column.into()),
        json_path: None,
        percents: None,
        missing: None,
    }
}

fn metric_ordered_terms() -> BTreeMap<String, Aggregation> {
    BTreeMap::from([(
        "types".into(),
        Aggregation::Terms {
            column: "tipas".into(),
            size: 20,
            segment_size: Some(100),
            min_doc_count: Some(1),
            missing: Some(json!("__missing__")),
            order: Some(BucketOrder {
                target: "total_value".into(),
                descending: true,
            }),
            aggregations: BTreeMap::from([(
                "total_value".into(),
                metric(Metric::Sum, "numatomaVerte"),
            )]),
        },
    )])
}

fn filtered_bucket() -> BTreeMap<String, Aggregation> {
    BTreeMap::from([(
        "valuable".into(),
        Aggregation::Filter {
            filter: Filter::Compare {
                column: "numatomaVerte".into(),
                operator: Comparison::GreaterOrEqual,
                value: json!(100_000),
            },
            aggregations: BTreeMap::from([(
                "stats".into(),
                metric(Metric::Stats, "numatomaVerte"),
            )]),
        },
    )])
}

fn top_hits_per_type() -> BTreeMap<String, Aggregation> {
    let hits = Aggregation::TopHits {
        size: 3,
        sort: vec![Sort {
            column: "redagavimoData".into(),
            json_path: None,
            json_type: None,
            descending: true,
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        }],
        columns: vec!["unikalusId".into(), "redagavimoData".into()],
    };
    let mut request = metric_ordered_terms();
    if let Some(Aggregation::Terms { aggregations, .. }) = request.get_mut("types") {
        aggregations.insert("latest".into(), hits);
    }
    request
}

fn distributed_request() -> BTreeMap<String, Aggregation> {
    metric_ordered_terms()
}

fn measure_aggregation(
    search: &SearchService,
    case: &AggregationCase,
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    search.aggregate("sutartys", case.filter.as_ref(), case.request.clone())?;
    let mut samples = Vec::with_capacity(iterations);
    let mut result_rows = 0;
    let mut last_result = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let result = std::hint::black_box(search.aggregate(
            "sutartys",
            case.filter.as_ref(),
            std::hint::black_box(case.request.clone()),
        )?);
        samples.push(started.elapsed());
        result_rows = aggregation_result_units(&result);
        last_result = Some(result);
    }
    let measurement = measurement(case.name, result_rows, samples);
    if let (Some(capture), Some(result)) = (capture, last_result) {
        capture.record(
            case.name,
            aggregation_sql("sutartys", case.filter.as_ref(), &case.request),
            &result,
            &measurement,
        )?;
    }
    Ok(measurement)
}

fn measure_distributed_collect(
    search: &SearchService,
    request: &BTreeMap<String, Aggregation>,
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    search.aggregate_intermediate("sutartys", None, request.clone())?;
    let mut samples = Vec::with_capacity(iterations);
    let mut last_payload = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let payload = std::hint::black_box(search.aggregate_intermediate(
            "sutartys",
            None,
            std::hint::black_box(request.clone()),
        )?);
        samples.push(started.elapsed());
        last_payload = Some(payload);
    }
    let measurement = measurement("distributed_aggregation_collect", 1, samples);
    if let (Some(capture), Some(payload)) = (capture, last_payload) {
        let result = json!({
            "result":"opaque mergeable Tantivy intermediate payload",
            "format":"tantivy-bincode-v1",
            "bytes":payload.len(),
        });
        capture.record(
            "distributed_aggregation_collect",
            format!(
                "{}\n-- Collect one shard's mergeable intermediate result.",
                aggregation_sql("sutartys", None, request)
            ),
            &result,
            &measurement,
        )?;
    }
    Ok(measurement)
}

fn measure_distributed_merge(
    search: &SearchService,
    request: &BTreeMap<String, Aggregation>,
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    let payload = search.aggregate_intermediate("sutartys", None, request.clone())?;
    let mut samples = Vec::with_capacity(iterations);
    let mut result_rows = 0;
    let mut last_result = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let result = std::hint::black_box(search.merge_aggregation_intermediates(
            "sutartys",
            std::hint::black_box(request.clone()),
            std::slice::from_ref(&payload),
        )?);
        samples.push(started.elapsed());
        result_rows = aggregation_result_units(&result);
        last_result = Some(result);
    }
    let measurement = measurement("distributed_aggregation_merge", result_rows, samples);
    if let (Some(capture), Some(result)) = (capture, last_result) {
        capture.record(
            "distributed_aggregation_merge",
            format!(
                "{}\n-- Merge 1 shard intermediate result.",
                aggregation_sql("sutartys", None, request)
            ),
            &result,
            &measurement,
        )?;
    }
    Ok(measurement)
}

fn aggregation_result_units(result: &Value) -> usize {
    result
        .as_object()
        .map(|aggregations| {
            aggregations
                .values()
                .map(|aggregation| match &aggregation["buckets"] {
                    Value::Array(buckets) => buckets.len(),
                    Value::Object(buckets) => buckets.len(),
                    _ => 1,
                })
                .sum()
        })
        .unwrap_or(0)
}
