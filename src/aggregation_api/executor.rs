use rayon::prelude::*;
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::query::EnableScoring;

use super::*;
use crate::segment_ranges::{collect_matching_docs_in_range, segment_doc_ranges};

const MAX_AGGREGATION_RANGE_WORKERS: usize = 2;

pub(crate) fn collect_aggregation_results(
    searcher: &Searcher,
    query: &dyn Query,
    index: &Index,
    request: Aggregations,
    pool: &rayon::ThreadPool,
) -> Result<AggregationResults> {
    let limits = AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(65_000));
    let collector = AggregationCollector::from_aggs(
        request.clone(),
        aggregation_context_with_limits(index, limits),
    );
    match collect_aggregation_ranges(searcher, query, &collector, pool) {
        Ok(Some(result)) => return Ok(result),
        Ok(None) => {}
        Err(error) if aggregation_memory_exceeded(&error) => {
            let collector = AggregationCollector::from_aggs(
                request,
                aggregation_context_with_limits(
                    index,
                    AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(65_000)),
                ),
            );
            return Ok(searcher.search(query, &collector)?);
        }
        Err(error) => return Err(error),
    }
    Ok(searcher.search(query, &collector)?)
}

fn collect_aggregation_ranges(
    searcher: &Searcher,
    query: &dyn Query,
    collector: &AggregationCollector,
    pool: &rayon::ThreadPool,
) -> Result<Option<AggregationResults>> {
    let [reader] = searcher.segment_readers() else {
        return Ok(None);
    };
    let ranges = segment_doc_ranges(reader.max_doc(), aggregation_pool_workers(pool));
    if ranges.len() < 2 {
        return Ok(None);
    }
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let results = pool.install(|| {
        ranges
            .into_par_iter()
            .map(|range| -> Result<_> {
                let mut child = collector.for_segment(0, reader)?;
                collect_matching_docs_in_range(&*weight, reader, range, |docs| {
                    child.collect_block(docs);
                })?;
                Ok(child.harvest())
            })
            .collect::<Vec<_>>()
    });
    let fruits = results.into_iter().collect::<Result<Vec<_>>>()?;
    Ok(Some(collector.merge_fruits(fruits)?))
}

/// Splits one large segment into document ranges, or independently executes top-level
/// aggregations when the segment is too small to amortize range collectors.
pub(crate) fn collect_standard_aggregations(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    index: &Index,
    aggregations: BTreeMap<String, Aggregation>,
    pool: &rayon::ThreadPool,
) -> Result<serde_json::Map<String, Value>> {
    match collect_intra_segment_aggregations(searcher, query, def, index, &aggregations, pool) {
        Ok(Some(result)) => return Ok(result),
        Ok(None) => {}
        Err(error) if aggregation_memory_exceeded(&error) => {
            return collect_group(searcher, query, def, index, aggregations, None);
        }
        Err(error) => return Err(error),
    }
    let worker_count = standard_aggregation_worker_count(searcher, aggregations.len(), pool);
    if worker_count == 1 {
        return collect_group(searcher, query, def, index, aggregations, None);
    }

    let groups = partition_aggregations(aggregations, worker_count);
    let limits = AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(10_000));
    let results = if let [reader] = searcher.segment_readers() {
        let docs = Arc::new(collect_matching_docs(searcher, query, reader)?);
        pool.install(|| {
            groups
                .into_par_iter()
                .map(|group| {
                    collect_group_from_docs(reader, def, index, group, limits.clone(), &docs)
                })
                .collect::<Vec<_>>()
        })
    } else {
        pool.install(|| {
            groups
                .into_par_iter()
                .map(|group| {
                    let limits = limits.clone();
                    collect_group(searcher, query, def, index, group, Some(limits))
                })
                .collect::<Vec<_>>()
        })
    };
    let mut combined = serde_json::Map::new();
    for result in results {
        combined.extend(result?);
    }
    Ok(combined)
}

fn collect_matching_docs(
    searcher: &Searcher,
    query: &dyn Query,
    reader: &SegmentReader,
) -> Result<Vec<DocId>> {
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut docs = Vec::new();
    collect_matching_docs_in_range(&*weight, reader, 0..reader.max_doc(), |block| {
        docs.extend_from_slice(block);
    })?;
    Ok(docs)
}

fn collect_group_from_docs(
    reader: &SegmentReader,
    def: &TableDef,
    index: &Index,
    aggregations: BTreeMap<String, Aggregation>,
    limits: AggregationLimitsGuard,
    docs: &[DocId],
) -> Result<serde_json::Map<String, Value>> {
    let request = compile_aggregations(def, &aggregations)?;
    let collector =
        AggregationCollector::from_aggs(request, aggregation_context_with_limits(index, limits));
    let mut child = collector.for_segment(0, reader)?;
    for block in docs.chunks(tantivy::COLLECT_BLOCK_BUFFER_LEN) {
        child.collect_block(block);
    }
    let value = serde_json::to_value(collector.merge_fruits(vec![child.harvest()])?)?;
    value
        .as_object()
        .cloned()
        .context("Tantivy aggregation response is not an object")
}

pub(crate) fn standard_aggregation_worker_count(
    searcher: &Searcher,
    aggregation_count: usize,
    pool: &rayon::ThreadPool,
) -> usize {
    if aggregation_count == 0 || searcher.segment_readers().len() != 1 {
        return 1;
    }
    let range_workers = aggregation_range_worker_count(searcher, pool);
    if range_workers > 1 {
        return range_workers;
    }
    if aggregation_count < 2 {
        return 1;
    }
    pool.current_num_threads().min(aggregation_count)
}

pub(crate) fn aggregation_range_worker_count(
    searcher: &Searcher,
    pool: &rayon::ThreadPool,
) -> usize {
    let [reader] = searcher.segment_readers() else {
        return 1;
    };
    segment_doc_ranges(reader.max_doc(), aggregation_pool_workers(pool))
        .len()
        .max(1)
}

fn aggregation_pool_workers(pool: &rayon::ThreadPool) -> usize {
    pool.current_num_threads()
        .min(MAX_AGGREGATION_RANGE_WORKERS)
}

fn aggregation_memory_exceeded(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains("memory limit was exceeded"))
}

fn collect_intra_segment_aggregations(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    index: &Index,
    aggregations: &BTreeMap<String, Aggregation>,
    pool: &rayon::ThreadPool,
) -> Result<Option<serde_json::Map<String, Value>>> {
    let [reader] = searcher.segment_readers() else {
        return Ok(None);
    };
    let ranges = segment_doc_ranges(reader.max_doc(), aggregation_pool_workers(pool));
    if ranges.len() < 2 {
        return Ok(None);
    }
    let request = compile_aggregations(def, aggregations)?;
    let limits = AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(10_000));
    let collector =
        AggregationCollector::from_aggs(request, aggregation_context_with_limits(index, limits));
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let results = pool.install(|| {
        ranges
            .into_par_iter()
            .map(|range| -> Result<_> {
                let mut child = collector.for_segment(0, reader)?;
                collect_matching_docs_in_range(&*weight, reader, range, |docs| {
                    child.collect_block(docs);
                })?;
                Ok(child.harvest())
            })
            .collect::<Vec<_>>()
    });
    let fruits = results.into_iter().collect::<Result<Vec<_>>>()?;
    let value = serde_json::to_value(collector.merge_fruits(fruits)?)?;
    Ok(Some(value.as_object().cloned().context(
        "Tantivy aggregation response is not an object",
    )?))
}

fn partition_aggregations(
    aggregations: BTreeMap<String, Aggregation>,
    worker_count: usize,
) -> Vec<BTreeMap<String, Aggregation>> {
    let mut groups = vec![BTreeMap::new(); worker_count];
    for (index, (name, aggregation)) in aggregations.into_iter().enumerate() {
        groups[index % worker_count].insert(name, aggregation);
    }
    groups
}

fn collect_group(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    index: &Index,
    aggregations: BTreeMap<String, Aggregation>,
    limits: Option<AggregationLimitsGuard>,
) -> Result<serde_json::Map<String, Value>> {
    let request = compile_aggregations(def, &aggregations)?;
    let context = limits.map_or_else(
        || aggregation_context(index),
        |limits| aggregation_context_with_limits(index, limits),
    );
    let collector = AggregationCollector::from_aggs(request, context);
    let value = serde_json::to_value(searcher.search(query, &collector)?)?;
    value
        .as_object()
        .cloned()
        .context("Tantivy aggregation response is not an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partitions_top_level_aggregations_without_losing_names() {
        let source = (0..7)
            .map(|index| {
                (
                    format!("aggregation_{index}"),
                    Aggregation::Filter {
                        filter: Filter::IsNull {
                            column: "value".into(),
                            negated: false,
                        },
                        aggregations: BTreeMap::new(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let groups = partition_aggregations(source, 4);
        assert_eq!(
            groups.iter().map(BTreeMap::len).collect::<Vec<_>>(),
            [2, 2, 2, 1]
        );
        assert_eq!(
            groups.into_iter().map(|group| group.len()).sum::<usize>(),
            7
        );
    }
}
