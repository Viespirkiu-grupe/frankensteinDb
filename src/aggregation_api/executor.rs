use rayon::prelude::*;
use tantivy::collector::{Collector, SegmentCollector};
use tantivy::query::EnableScoring;

use super::*;
use crate::segment_ranges::{collect_matching_docs_in_range, segment_doc_ranges};

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
    if let Some(result) =
        collect_intra_segment_aggregations(searcher, query, def, index, &aggregations, pool)?
    {
        return Ok(result);
    }
    let worker_count = standard_aggregation_worker_count(searcher, aggregations.len(), pool);
    if worker_count == 1 {
        return collect_group(searcher, query, def, index, aggregations, None);
    }

    let groups = partition_aggregations(aggregations, worker_count);
    let limits = AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(10_000));
    let results = pool.install(|| {
        groups
            .into_par_iter()
            .map(|group| {
                let limits = limits.clone();
                collect_group(searcher, query, def, index, group, Some(limits))
            })
            .collect::<Vec<_>>()
    });
    let mut combined = serde_json::Map::new();
    for result in results {
        combined.extend(result?);
    }
    Ok(combined)
}

pub(crate) fn standard_aggregation_worker_count(
    searcher: &Searcher,
    aggregation_count: usize,
    pool: &rayon::ThreadPool,
) -> usize {
    if aggregation_count == 0 || searcher.segment_readers().len() != 1 {
        return 1;
    }
    let range_workers = segment_doc_ranges(
        searcher.segment_readers()[0].max_doc(),
        pool.current_num_threads(),
    )
    .len();
    if range_workers > 1 {
        return range_workers;
    }
    if aggregation_count < 2 {
        return 1;
    }
    pool.current_num_threads().min(aggregation_count)
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
    let ranges = segment_doc_ranges(reader.max_doc(), pool.current_num_threads());
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
