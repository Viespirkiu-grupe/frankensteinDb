use tantivy::SegmentOrdinal;
use tantivy::collector::{Collector, SegmentCollector};

use super::*;

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

#[derive(Clone, Debug)]
struct ScoredHit {
    key: Vec<OwnedValue>,
    score: f32,
    address: DocAddress,
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
            top_hits: BufferedScoredTopK::new(self.top_n, self.fields.clone()),
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
    top_hits: BufferedScoredTopK,
    min_score: Option<f32>,
}

impl SegmentCollector for ScoredTopKSegmentCollector {
    type Fruit = Vec<ScoredHit>;

    fn collect(&mut self, doc: DocId, score: Score) {
        if self.min_score.is_some_and(|minimum| score < minimum) {
            return;
        }
        let key = self
            .fields
            .iter()
            .zip(&self.readers)
            .map(|(field, reader)| match field {
                ScoredSortField::Score(_) => OwnedValue::F64(score as f64),
                ScoredSortField::Fast(_) => {
                    let reader = reader.as_ref().expect("fast sort reader");
                    reader.sort_owned_value(doc)
                }
            })
            .collect();
        self.top_hits.push(ScoredHit {
            key,
            score,
            address: DocAddress::new(self.segment_ord, doc),
        });
    }

    fn harvest(self) -> Self::Fruit {
        self.top_hits.finish()
    }
}

struct BufferedScoredTopK {
    hits: Vec<ScoredHit>,
    top_n: usize,
    threshold: Option<ScoredHit>,
    fields: Vec<ScoredSortField>,
}

impl BufferedScoredTopK {
    fn new(top_n: usize, fields: Vec<ScoredSortField>) -> Self {
        Self {
            hits: Vec::with_capacity(top_n.max(1).saturating_mul(10)),
            top_n,
            threshold: None,
            fields,
        }
    }

    fn push(&mut self, hit: ScoredHit) {
        if self.top_n == 0
            || self
                .threshold
                .as_ref()
                .is_some_and(|threshold| scored_output_order(&hit, threshold, &self.fields).is_ge())
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
            scored_output_order(left, right, &self.fields)
        });
        self.hits.truncate(self.top_n);
        self.threshold = self
            .hits
            .iter()
            .max_by(|left, right| scored_output_order(left, right, &self.fields))
            .cloned();
    }

    fn finish(mut self) -> Vec<ScoredHit> {
        self.truncate();
        self.hits
    }
}

fn scored_output_order(
    left: &ScoredHit,
    right: &ScoredHit,
    fields: &[ScoredSortField],
) -> std::cmp::Ordering {
    for ((left, right), field) in left.key.iter().zip(&right.key).zip(fields) {
        let order = match field {
            ScoredSortField::Score(order)
            | ScoredSortField::Fast(NativeSortField { order, .. }) => *order,
        };
        let comparison = owned_value_order(left, right);
        let comparison = if order == Order::Asc {
            comparison
        } else {
            comparison.reverse()
        };
        if !comparison.is_eq() {
            return comparison;
        }
    }
    left.address.cmp(&right.address)
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
