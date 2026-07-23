use tantivy::SegmentOrdinal;
use tantivy::collector::{Collector, SegmentCollector};

use super::*;

mod segment;
use segment::*;

#[derive(Clone)]
pub(crate) enum ScoredSortField {
    Score(Order),
    Fast(NativeSortField),
}

pub(crate) struct ScoredSort {
    fields: Vec<ScoredSortField>,
    min_score: Option<f32>,
    requires_scoring: bool,
}

pub(crate) fn typed_scored_sort(
    request: &ReadRequest,
    def: &TableDef,
    order: &[OrderSpec],
) -> Option<ScoredSort> {
    let needs_scores = request.min_score.is_some()
        || request
            .projection
            .iter()
            .any(|item| matches!(item, Projection::Score { .. }))
        || order
            .iter()
            .any(|spec| spec.key.eq_ignore_ascii_case("_score"));
    let native_score_order =
        order.len() == 1 && order[0].key.eq_ignore_ascii_case("_score") && !order[0].asc;
    if request.min_score.is_none() && (order.is_empty() || native_score_order) {
        return None;
    }
    if order.is_empty() && !needs_scores {
        return None;
    }
    let mut fields = Vec::with_capacity(order.len().max(1));
    if order.is_empty() {
        fields.push(ScoredSortField::Score(Order::Desc));
    } else {
        for spec in order {
            if spec.key.eq_ignore_ascii_case("_score") {
                fields.push(ScoredSortField::Score(if spec.asc {
                    Order::Asc
                } else {
                    Order::Desc
                }));
                continue;
            }
            fields.push(ScoredSortField::Fast(native_sort_field(def, spec)?));
        }
    }
    Some(ScoredSort {
        fields,
        min_score: request.min_score,
        requires_scoring: needs_scores,
    })
}

fn native_sort_field(def: &TableDef, spec: &OrderSpec) -> Option<NativeSortField> {
    if let Some(data_type) = spec.json_type {
        return Some(NativeSortField {
            field: spec.key.clone(),
            data_type: json_path_column_type(data_type),
            order: sort_order(spec.asc),
            geo_distance_from: None,
            geo_distance_mode: GeoDistanceMode::Min,
        });
    }
    let column = column(def, &spec.key).ok()?;
    let geo_sort = spec.geo_distance_from.is_some();
    if (column.data_type.is_array() && !geo_sort)
        || matches!(column.data_type, ColumnType::Json | ColumnType::Facet)
    {
        return None;
    }
    Some(NativeSortField {
        field: aggregation_field(column),
        data_type: column.data_type,
        order: sort_order(spec.asc),
        geo_distance_from: spec.geo_distance_from,
        geo_distance_mode: spec.geo_distance_mode,
    })
}

fn sort_order(ascending: bool) -> Order {
    if ascending { Order::Asc } else { Order::Desc }
}

pub(crate) fn collect_scored_top_k(
    searcher: &Searcher,
    query: &dyn Query,
    sort: &ScoredSort,
    limit: usize,
    offset: usize,
) -> Result<Vec<(f32, DocAddress)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let collector = ScoredTopKCollector {
        fields: sort.fields.clone(),
        top_n: limit.saturating_add(offset),
        offset,
        min_score: sort.min_score,
        requires_scoring: sort.requires_scoring,
    };
    Ok(searcher.search(query, &collector)?)
}

#[derive(Debug)]
struct ScoredHit {
    key: ScoredKey,
    score: f32,
    address: DocAddress,
}

#[derive(Debug)]
enum ScoredKey {
    One(OwnedValue),
    Two(OwnedValue, OwnedValue),
    Many(Vec<OwnedValue>),
}

struct ScoredTopKCollector {
    fields: Vec<ScoredSortField>,
    top_n: usize,
    offset: usize,
    min_score: Option<f32>,
    requires_scoring: bool,
}

impl Collector for ScoredTopKCollector {
    type Child = ScoredTopKSegmentCollector;
    type Fruit = Vec<(f32, DocAddress)>;

    fn for_segment(
        &self,
        segment_ord: SegmentOrdinal,
        segment: &SegmentReader,
    ) -> tantivy::Result<Self::Child> {
        let readers = self
            .fields
            .iter()
            .map(|field| match field {
                ScoredSortField::Score(_) => Ok(None),
                ScoredSortField::Fast(field) => {
                    sort_fast_values(segment.fast_fields(), field).map(Some)
                }
            })
            .collect::<tantivy::Result<Vec<_>>>()?;
        Ok(ScoredTopKSegmentCollector {
            segment_ord,
            fields: self.fields.clone(),
            readers,
            top_hits: BufferedSegmentScoredTopK::new(self.top_n, self.fields.clone()),
            min_score: self.min_score,
        })
    }

    fn requires_scoring(&self) -> bool {
        self.requires_scoring
    }

    fn merge_fruits(
        &self,
        segment_fruits: Vec<<Self::Child as SegmentCollector>::Fruit>,
    ) -> tantivy::Result<Self::Fruit> {
        let mut hits = segment_fruits.into_iter().flatten().collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| scored_output_order(left, right, &self.fields));
        hits.truncate(self.top_n);
        Ok(hits
            .into_iter()
            .skip(self.offset)
            .map(|hit| (hit.score, hit.address))
            .collect())
    }
}

struct ScoredTopKSegmentCollector {
    segment_ord: SegmentOrdinal,
    fields: Vec<ScoredSortField>,
    readers: Vec<Option<FastValues>>,
    top_hits: BufferedSegmentScoredTopK,
    min_score: Option<f32>,
}

impl SegmentCollector for ScoredTopKSegmentCollector {
    type Fruit = Vec<ScoredHit>;

    fn collect(&mut self, doc: DocId, score: Score) {
        if self.min_score.is_some_and(|minimum| score < minimum) {
            return;
        }
        let value = |index: usize| {
            segment_scored_value(
                &self.fields[index],
                self.readers[index].as_ref(),
                doc,
                score,
            )
        };
        let key = match self.fields.len() {
            1 => SegmentScoredKey::One(value(0)),
            2 => SegmentScoredKey::Two(value(0), value(1)),
            _ => SegmentScoredKey::Many((0..self.fields.len()).map(value).collect()),
        };
        self.top_hits.push(SegmentScoredHit {
            key,
            score,
            address: DocAddress::new(self.segment_ord, doc),
        });
    }

    fn harvest(self) -> Self::Fruit {
        self.top_hits
            .finish()
            .into_iter()
            .map(|hit| ScoredHit {
                key: materialize_scored_key(hit.key, &self.fields, &self.readers),
                score: hit.score,
                address: hit.address,
            })
            .collect()
    }
}

fn scored_output_order(
    left: &ScoredHit,
    right: &ScoredHit,
    fields: &[ScoredSortField],
) -> std::cmp::Ordering {
    scored_key_order(&left.key, &right.key, fields).then_with(|| left.address.cmp(&right.address))
}

fn scored_key_order(
    left: &ScoredKey,
    right: &ScoredKey,
    fields: &[ScoredSortField],
) -> std::cmp::Ordering {
    match (left, right) {
        (ScoredKey::One(left), ScoredKey::One(right)) => {
            compare_scored_value(left, right, &fields[0])
        }
        (ScoredKey::Two(left_first, left_second), ScoredKey::Two(right_first, right_second)) => {
            compare_scored_value(left_first, right_first, &fields[0])
                .then_with(|| compare_scored_value(left_second, right_second, &fields[1]))
        }
        (ScoredKey::Many(left), ScoredKey::Many(right)) => left
            .iter()
            .zip(right)
            .zip(fields)
            .map(|((left, right), field)| compare_scored_value(left, right, field))
            .find(|order| !order.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => unreachable!("one collector uses one scored-key shape"),
    }
}

fn compare_scored_value(
    left: &OwnedValue,
    right: &OwnedValue,
    field: &ScoredSortField,
) -> std::cmp::Ordering {
    let comparison = owned_value_order(left, right);
    if scored_field_order(field) == Order::Asc {
        comparison
    } else {
        comparison.reverse()
    }
}

fn scored_field_order(field: &ScoredSortField) -> Order {
    match field {
        ScoredSortField::Score(order) | ScoredSortField::Fast(NativeSortField { order, .. }) => {
            *order
        }
    }
}

fn owned_value_order(left: &OwnedValue, right: &OwnedValue) -> std::cmp::Ordering {
    match (left, right) {
        (OwnedValue::Null, OwnedValue::Null) => std::cmp::Ordering::Equal,
        (OwnedValue::Null, _) => std::cmp::Ordering::Greater,
        (_, OwnedValue::Null) => std::cmp::Ordering::Less,
        (OwnedValue::I64(left), OwnedValue::I64(right)) => left.cmp(right),
        (OwnedValue::U64(left), OwnedValue::U64(right)) => left.cmp(right),
        (OwnedValue::F64(left), OwnedValue::F64(right)) => left.total_cmp(right),
        (OwnedValue::Bool(left), OwnedValue::Bool(right)) => left.cmp(right),
        (OwnedValue::Date(left), OwnedValue::Date(right)) => left.cmp(right),
        (OwnedValue::IpAddr(left), OwnedValue::IpAddr(right)) => left.cmp(right),
        (OwnedValue::Str(left), OwnedValue::Str(right)) => left.cmp(right),
        (OwnedValue::Bytes(left), OwnedValue::Bytes(right)) => left.cmp(right),
        _ => format!("{left:?}").cmp(&format!("{right:?}")),
    }
}
