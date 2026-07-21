use tantivy::collector::{Collector, SegmentCollector};
use tantivy::{COLLECT_BLOCK_BUFFER_LEN, SegmentOrdinal};

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockSortKey {
    One(Option<u64>),
    Two(Option<u64>, Option<u64>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlockHit {
    key: BlockSortKey,
    address: DocAddress,
}

pub(super) fn collect_block_top_k(
    searcher: &Searcher,
    query: &dyn Query,
    sort: &NativeSort,
    limit: usize,
    offset: usize,
) -> Result<Vec<(f32, DocAddress)>> {
    let collector = BlockTopKCollector {
        fields: sort.fields.clone(),
        top_n: limit.saturating_add(offset),
        offset,
    };
    Ok(searcher
        .search(query, &collector)?
        .into_iter()
        .map(|address| (0.0, address))
        .collect())
}

struct BlockTopKCollector {
    fields: Vec<NativeSortField>,
    top_n: usize,
    offset: usize,
}

impl Collector for BlockTopKCollector {
    type Child = BlockTopKSegmentCollector;
    type Fruit = Vec<DocAddress>;

    fn for_segment(
        &self,
        segment_ord: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let readers = self
            .fields
            .iter()
            .map(|field| {
                segment
                    .fast_fields()
                    .u64_lenient(&field.field)?
                    .map(|(column, _)| column)
                    .ok_or_else(|| {
                        tantivy::TantivyError::SchemaError(format!(
                            "missing fast field: {}",
                            field.field
                        ))
                    })
            })
            .collect::<tantivy::Result<Vec<_>>>()?;
        Ok(BlockTopKSegmentCollector::new(
            segment_ord,
            readers,
            self.fields.iter().map(|field| field.order).collect(),
            self.top_n,
        ))
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<<Self::Child as SegmentCollector>::Fruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let orders = self
            .fields
            .iter()
            .map(|field| field.order)
            .collect::<Vec<_>>();
        let mut hits = segment_fruits.into_iter().flatten().collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| output_order(left, right, &orders));
        hits.truncate(self.top_n);
        Ok(hits
            .into_iter()
            .skip(self.offset)
            .map(|hit| hit.address)
            .collect())
    }
}

struct BlockTopKSegmentCollector {
    segment_ord: SegmentOrdinal,
    readers: Vec<Column<u64>>,
    values: Vec<[Option<u64>; COLLECT_BLOCK_BUFFER_LEN]>,
    top_hits: BufferedTopK,
}

impl BlockTopKSegmentCollector {
    fn new(
        segment_ord: SegmentOrdinal,
        readers: Vec<Column<u64>>,
        orders: Vec<Order>,
        top_n: usize,
    ) -> Self {
        let values = readers
            .iter()
            .map(|_| [None; COLLECT_BLOCK_BUFFER_LEN])
            .collect();
        Self {
            segment_ord,
            readers,
            values,
            top_hits: BufferedTopK::new(top_n, orders),
        }
    }

    fn push_doc(&mut self, doc: DocId, first: Option<u64>, second: Option<u64>) {
        let key = if self.readers.len() == 1 {
            BlockSortKey::One(first)
        } else {
            BlockSortKey::Two(first, second)
        };
        self.top_hits.push(BlockHit {
            key,
            address: DocAddress::new(self.segment_ord, doc),
        });
    }
}

impl SegmentCollector for BlockTopKSegmentCollector {
    type Fruit = Vec<BlockHit>;

    fn collect_block(&mut self, docs: &[DocId]) {
        for (reader, values) in self.readers.iter().zip(&mut self.values) {
            reader.first_vals(docs, &mut values[..docs.len()]);
        }
        for (index, doc) in docs.iter().copied().enumerate() {
            let first = self.values[0][index];
            let second = self
                .values
                .get(1)
                .map(|values| values[index])
                .unwrap_or(None);
            self.push_doc(doc, first, second);
        }
    }

    fn collect(&mut self, doc: DocId, _score: Score) {
        let first = self.readers[0].first(doc);
        let second = self.readers.get(1).and_then(|reader| reader.first(doc));
        self.push_doc(doc, first, second);
    }

    fn harvest(self) -> Self::Fruit {
        self.top_hits.finish()
    }
}

struct BufferedTopK {
    hits: Vec<BlockHit>,
    top_n: usize,
    threshold: Option<BlockHit>,
    orders: Vec<Order>,
}

impl BufferedTopK {
    fn new(top_n: usize, orders: Vec<Order>) -> Self {
        Self {
            hits: Vec::with_capacity(top_n.max(1).saturating_mul(10)),
            top_n,
            threshold: None,
            orders,
        }
    }

    fn push(&mut self, hit: BlockHit) {
        if self.top_n == 0
            || self
                .threshold
                .is_some_and(|threshold| output_order(&hit, &threshold, &self.orders).is_ge())
        {
            return;
        }
        if self.hits.len() == self.hits.capacity() {
            self.truncate();
        }
        self.hits.push(hit);
    }

    fn truncate(&mut self) {
        if self.hits.len() <= self.top_n {
            return;
        }
        self.hits.select_nth_unstable_by(self.top_n, |left, right| {
            output_order(left, right, &self.orders)
        });
        self.hits.truncate(self.top_n);
        self.threshold = self
            .hits
            .iter()
            .copied()
            .max_by(|left, right| output_order(left, right, &self.orders));
    }

    fn finish(mut self) -> Vec<BlockHit> {
        self.truncate();
        self.hits
    }
}

fn output_order(left: &BlockHit, right: &BlockHit, orders: &[Order]) -> std::cmp::Ordering {
    compare_key(left.key, right.key, orders).then_with(|| left.address.cmp(&right.address))
}

fn compare_key(left: BlockSortKey, right: BlockSortKey, orders: &[Order]) -> std::cmp::Ordering {
    match (left, right) {
        (BlockSortKey::One(left), BlockSortKey::One(right)) => {
            compare_value(left, right, orders[0])
        }
        (BlockSortKey::Two(left1, left2), BlockSortKey::Two(right1, right2)) => {
            compare_value(left1, right1, orders[0])
                .then_with(|| compare_value(left2, right2, orders[1]))
        }
        _ => unreachable!("one collector uses one sort-key shape"),
    }
}

fn compare_value(left: Option<u64>, right: Option<u64>, order: Order) -> std::cmp::Ordering {
    let ascending = match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => left.cmp(&right),
    };
    match order {
        Order::Asc => ascending,
        Order::Desc => ascending.reverse(),
    }
}
