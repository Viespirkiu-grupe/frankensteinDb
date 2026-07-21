use rayon::prelude::*;

use super::*;

/// Collects independent top-level aggregations concurrently when one segment would otherwise
/// force the entire request through one Tantivy worker.
pub(crate) fn collect_standard_aggregations(
    searcher: &Searcher,
    query: &dyn Query,
    def: &TableDef,
    index: &Index,
    aggregations: BTreeMap<String, Aggregation>,
    pool: &rayon::ThreadPool,
) -> Result<serde_json::Map<String, Value>> {
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
    if searcher.segment_readers().len() != 1 || aggregation_count < 2 {
        return 1;
    }
    pool.current_num_threads().min(aggregation_count)
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
