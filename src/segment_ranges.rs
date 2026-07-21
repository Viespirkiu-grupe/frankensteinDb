use std::ops::Range;

use tantivy::query::Weight;
use tantivy::{COLLECT_BLOCK_BUFFER_LEN, DocId, DocSet, SegmentReader, TERMINATED};

/// Avoids parallel setup for ranges too small to amortize independent scorers and collectors.
#[cfg(not(test))]
pub(crate) const MIN_DOCS_PER_RANGE: usize = 250_000;
#[cfg(test)]
pub(crate) const MIN_DOCS_PER_RANGE: usize = 100;

pub(crate) fn segment_doc_ranges(max_doc: DocId, requested_workers: usize) -> Vec<Range<DocId>> {
    if max_doc == 0 {
        return Vec::new();
    }
    let useful_workers = (max_doc as usize)
        .div_ceil(MIN_DOCS_PER_RANGE)
        .max(1)
        .min(requested_workers.max(1));
    let chunk_size = (max_doc as usize).div_ceil(useful_workers);
    (0..useful_workers)
        .map(|worker| {
            let start = (worker * chunk_size) as DocId;
            let end = max_doc.min(((worker + 1) * chunk_size) as DocId);
            start..end
        })
        .filter(|range| !range.is_empty())
        .collect()
}

pub(crate) fn collect_matching_docs_in_range(
    weight: &dyn Weight,
    reader: &SegmentReader,
    range: Range<DocId>,
    mut collect_block: impl FnMut(&[DocId]),
) -> tantivy::Result<()> {
    let mut scorer = weight.scorer(reader, 1.0)?;
    let mut doc = scorer.doc();
    if doc < range.start {
        doc = scorer.seek(range.start);
    }
    let mut docs = [0; COLLECT_BLOCK_BUFFER_LEN];
    let mut length = 0;
    while doc != TERMINATED && doc < range.end {
        if reader
            .alive_bitset()
            .is_none_or(|alive| alive.is_alive(doc))
        {
            docs[length] = doc;
            length += 1;
            if length == docs.len() {
                collect_block(&docs);
                length = 0;
            }
        }
        doc = scorer.advance();
    }
    if length > 0 {
        collect_block(&docs[..length]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_cover_each_document_once() {
        let ranges = segment_doc_ranges(1_000_003, 4);
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end, 1_000_003);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        assert_eq!(
            ranges.iter().map(|range| range.len()).sum::<usize>(),
            1_000_003
        );
    }

    #[test]
    fn small_segments_remain_single_range() {
        assert_eq!(segment_doc_ranges(100, 8), vec![0..100]);
        assert!(segment_doc_ranges(0, 8).is_empty());
    }
}
