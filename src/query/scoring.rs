use tantivy::query::{EnableScoring, Explanation, Query, Scorer, Weight};
use tantivy::{DocId, DocSet, Score, SegmentReader, Term};

/// Prevents Tantivy's term-union specialization from replacing a custom score combiner.
///
/// Tantivy 0.26's boolean weight uses a sum-oriented Block-WAND path when every child exposes a
/// `TermScorer`. Hiding that concrete scorer preserves `DisjunctionMaxQuery` semantics while still
/// delegating document iteration and BM25 scoring to the original scorer.
#[derive(Debug)]
pub(crate) struct OpaqueScoreQuery {
    inner: Box<dyn Query>,
}

impl OpaqueScoreQuery {
    pub(crate) fn new(inner: Box<dyn Query>) -> Self {
        Self { inner }
    }
}

impl Clone for OpaqueScoreQuery {
    fn clone(&self) -> Self {
        Self::new(self.inner.box_clone())
    }
}

impl Query for OpaqueScoreQuery {
    fn weight(&self, enable_scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(OpaqueScoreWeight(
            self.inner.weight(enable_scoring)?,
        )))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.inner.query_terms(visitor);
    }
}

struct OpaqueScoreWeight(Box<dyn Weight>);

impl Weight for OpaqueScoreWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(OpaqueScorer(self.0.scorer(reader, boost)?)))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        self.0.explain(reader, doc)
    }
}

struct OpaqueScorer(Box<dyn Scorer>);

impl DocSet for OpaqueScorer {
    fn advance(&mut self) -> DocId {
        self.0.advance()
    }

    fn seek(&mut self, target: DocId) -> DocId {
        self.0.seek(target)
    }

    fn doc(&self) -> DocId {
        self.0.doc()
    }

    fn size_hint(&self) -> u32 {
        self.0.size_hint()
    }

    fn cost(&self) -> u64 {
        self.0.cost()
    }
}

impl Scorer for OpaqueScorer {
    fn score(&mut self) -> Score {
        self.0.score()
    }
}
