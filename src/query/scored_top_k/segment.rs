use super::*;

#[derive(Clone, Copy, Debug)]
pub(super) enum SegmentScoredValue {
    Score(f32),
    Fast(Option<u64>),
    Ip(Option<std::net::Ipv6Addr>),
}

#[derive(Clone, Debug)]
pub(super) enum SegmentScoredKey {
    One(SegmentScoredValue),
    Two(SegmentScoredValue, SegmentScoredValue),
    Many(Vec<SegmentScoredValue>),
}

#[derive(Clone, Debug)]
pub(super) struct SegmentScoredHit {
    pub(super) key: SegmentScoredKey,
    pub(super) score: f32,
    pub(super) address: DocAddress,
}

pub(super) struct BufferedSegmentScoredTopK {
    hits: Vec<SegmentScoredHit>,
    top_n: usize,
    threshold: Option<SegmentScoredHit>,
    fields: Vec<ScoredSortField>,
}

impl BufferedSegmentScoredTopK {
    pub(super) fn new(top_n: usize, fields: Vec<ScoredSortField>) -> Self {
        Self {
            hits: Vec::with_capacity(top_n.max(1).saturating_mul(10)),
            top_n,
            threshold: None,
            fields,
        }
    }

    pub(super) fn push(&mut self, hit: SegmentScoredHit) {
        if self.top_n == 0
            || self.threshold.as_ref().is_some_and(|threshold| {
                segment_scored_order(&hit, threshold, &self.fields).is_ge()
            })
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
            segment_scored_order(left, right, &self.fields)
        });
        self.hits.truncate(self.top_n);
        self.threshold = self
            .hits
            .iter()
            .max_by(|left, right| segment_scored_order(left, right, &self.fields))
            .cloned();
    }

    pub(super) fn finish(mut self) -> Vec<SegmentScoredHit> {
        self.truncate();
        self.hits
    }
}

pub(super) fn segment_scored_value(
    field: &ScoredSortField,
    reader: Option<&FastValues>,
    doc: DocId,
    score: Score,
) -> SegmentScoredValue {
    match field {
        ScoredSortField::Score(_) => SegmentScoredValue::Score(score),
        ScoredSortField::Fast(_) => match reader.expect("fast sort reader") {
            FastValues::Ip(values) => SegmentScoredValue::Ip(values.first(doc)),
            values => SegmentScoredValue::Fast(values.segment_sort_value(doc)),
        },
    }
}

pub(super) fn materialize_scored_key(
    key: SegmentScoredKey,
    fields: &[ScoredSortField],
    readers: &[Option<FastValues>],
) -> ScoredKey {
    let materialize = |index: usize, value: SegmentScoredValue| {
        materialize_scored_value(value, &fields[index], readers[index].as_ref())
    };
    match key {
        SegmentScoredKey::One(value) => ScoredKey::One(materialize(0, value)),
        SegmentScoredKey::Two(first, second) => {
            ScoredKey::Two(materialize(0, first), materialize(1, second))
        }
        SegmentScoredKey::Many(values) => ScoredKey::Many(
            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| materialize(index, value))
                .collect(),
        ),
    }
}

fn materialize_scored_value(
    value: SegmentScoredValue,
    field: &ScoredSortField,
    reader: Option<&FastValues>,
) -> OwnedValue {
    match (value, field) {
        (SegmentScoredValue::Score(score), ScoredSortField::Score(_)) => {
            OwnedValue::F64(score as f64)
        }
        (SegmentScoredValue::Fast(value), ScoredSortField::Fast(_)) => {
            reader.expect("fast sort reader").global_sort_value(value)
        }
        (SegmentScoredValue::Ip(value), ScoredSortField::Fast(_)) => {
            value.map(OwnedValue::IpAddr).unwrap_or(OwnedValue::Null)
        }
        _ => unreachable!("segment key matches its sort field"),
    }
}

fn segment_scored_order(
    left: &SegmentScoredHit,
    right: &SegmentScoredHit,
    fields: &[ScoredSortField],
) -> std::cmp::Ordering {
    segment_key_order(&left.key, &right.key, fields).then_with(|| left.address.cmp(&right.address))
}

fn segment_key_order(
    left: &SegmentScoredKey,
    right: &SegmentScoredKey,
    fields: &[ScoredSortField],
) -> std::cmp::Ordering {
    match (left, right) {
        (SegmentScoredKey::One(left), SegmentScoredKey::One(right)) => {
            compare_segment_value(*left, *right, &fields[0])
        }
        (
            SegmentScoredKey::Two(left_first, left_second),
            SegmentScoredKey::Two(right_first, right_second),
        ) => compare_segment_value(*left_first, *right_first, &fields[0])
            .then_with(|| compare_segment_value(*left_second, *right_second, &fields[1])),
        (SegmentScoredKey::Many(left), SegmentScoredKey::Many(right)) => left
            .iter()
            .zip(right)
            .zip(fields)
            .map(|((left, right), field)| compare_segment_value(*left, *right, field))
            .find(|order| !order.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => unreachable!("one collector uses one scored-key shape"),
    }
}

fn compare_segment_value(
    left: SegmentScoredValue,
    right: SegmentScoredValue,
    field: &ScoredSortField,
) -> std::cmp::Ordering {
    let comparison = match (left, right) {
        (SegmentScoredValue::Score(left), SegmentScoredValue::Score(right)) => {
            left.total_cmp(&right)
        }
        (SegmentScoredValue::Fast(left), SegmentScoredValue::Fast(right)) => {
            optional_value_order(left, right)
        }
        (SegmentScoredValue::Ip(left), SegmentScoredValue::Ip(right)) => {
            optional_value_order(left, right)
        }
        _ => unreachable!("segment values match their sort field"),
    };
    if scored_field_order(field) == Order::Asc {
        comparison
    } else {
        comparison.reverse()
    }
}

fn optional_value_order<T: Ord>(left: Option<T>, right: Option<T>) -> std::cmp::Ordering {
    match (left, right) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(left), Some(right)) => left.cmp(&right),
    }
}
