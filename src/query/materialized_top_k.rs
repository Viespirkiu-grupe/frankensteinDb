use rayon::prelude::*;
use tantivy::query::EnableScoring;

use super::*;
use crate::segment_ranges::{searcher_doc_ranges, visit_matching_docs_in_range};

struct MaterializedHit {
    row: ResultRow,
    keys: Vec<Value>,
    address: DocAddress,
}

/// Streams fallback-sort rows through bounded per-range buffers instead of materializing all hits.
pub(crate) fn collect_materialized_top_k(
    searcher: &Searcher,
    query: &dyn Query,
    columns: &[&ColumnDef],
    order: &[OrderSpec],
    limit: usize,
    offset: usize,
    pool: &rayon::ThreadPool,
) -> Result<Vec<ResultRow>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let top_n = limit.saturating_add(offset);
    let column_indices = Arc::new(
        columns
            .iter()
            .enumerate()
            .map(|(index, column)| (column.name.to_ascii_lowercase(), index))
            .collect::<HashMap<_, _>>(),
    );
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let results = pool.install(|| {
        searcher_doc_ranges(searcher, pool.current_num_threads())
            .into_par_iter()
            .map(|work| -> Result<_> {
                let readers = segment_fast_readers(searcher, work.segment_ord, columns)?;
                let mut buffer = MaterializedBuffer::new(top_n, order);
                visit_matching_docs_in_range(&*weight, work.reader, work.range, |doc| {
                    let values = readers
                        .iter()
                        .map(|reader| reader.value(doc))
                        .collect::<Result<Vec<_>>>()?;
                    let row = ResultRow {
                        values,
                        columns: Arc::clone(&column_indices),
                        score: 0.0,
                    };
                    let keys = order
                        .iter()
                        .map(|spec| sort_row_value(&row, spec))
                        .collect();
                    buffer.push(MaterializedHit {
                        row,
                        keys,
                        address: DocAddress::new(work.segment_ord, doc),
                    });
                    Ok(true)
                })?;
                Ok(buffer.finish())
            })
            .collect::<Vec<_>>()
    });
    let mut hits = results
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    hits.sort_unstable_by(|left, right| hit_order(left, right, order));
    hits.truncate(top_n);
    Ok(hits.into_iter().skip(offset).map(|hit| hit.row).collect())
}

struct MaterializedBuffer<'a> {
    hits: Vec<MaterializedHit>,
    top_n: usize,
    order: &'a [OrderSpec],
}

impl<'a> MaterializedBuffer<'a> {
    fn new(top_n: usize, order: &'a [OrderSpec]) -> Self {
        Self {
            hits: Vec::with_capacity(top_n.max(1).saturating_mul(2)),
            top_n,
            order,
        }
    }

    fn push(&mut self, hit: MaterializedHit) {
        self.hits.push(hit);
        if self.hits.len() == self.hits.capacity() {
            self.truncate();
        }
    }

    fn truncate(&mut self) {
        if self.hits.len() <= self.top_n {
            return;
        }
        self.hits
            .select_nth_unstable_by(self.top_n, |left, right| hit_order(left, right, self.order));
        self.hits.truncate(self.top_n);
    }

    fn finish(mut self) -> Vec<MaterializedHit> {
        self.truncate();
        self.hits
    }
}

fn hit_order(
    left: &MaterializedHit,
    right: &MaterializedHit,
    order: &[OrderSpec],
) -> std::cmp::Ordering {
    compare_ordered_values(&left.keys, &right.keys, order)
        .then_with(|| left.address.cmp(&right.address))
}
