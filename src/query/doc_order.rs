use tantivy::query::EnableScoring;

use super::*;
use crate::segment_ranges::visit_matching_docs_in_range;

/// Returns the requested window in stable segment/document order without scoring every match.
pub(crate) fn collect_doc_order_top_k(
    searcher: &Searcher,
    query: &dyn Query,
    limit: usize,
    offset: usize,
) -> Result<Vec<(f32, DocAddress)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let weight = query.weight(EnableScoring::disabled_from_searcher(searcher))?;
    let mut skipped = 0usize;
    let mut addresses = Vec::with_capacity(limit);
    for (segment_ord, reader) in searcher.segment_readers().iter().enumerate() {
        visit_matching_docs_in_range(&*weight, reader, 0..reader.max_doc(), |doc| {
            if skipped < offset {
                skipped += 1;
            } else if addresses.len() < limit {
                addresses.push((0.0, DocAddress::new(segment_ord as u32, doc)));
            }
            Ok(addresses.len() < limit)
        })?;
        if addresses.len() == limit {
            break;
        }
    }
    Ok(addresses)
}
