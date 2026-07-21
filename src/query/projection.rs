use super::*;

pub(crate) fn row_value<'a>(row: &'a ResultRow, name: &str) -> Option<&'a Value> {
    row.values
        .iter()
        .find(|(column, _)| column.eq_ignore_ascii_case(name))
        .map(|(_, value)| value)
}

pub(crate) fn sort_source(rows: &mut [ResultRow], order: &[OrderSpec]) -> Result<()> {
    for spec in order.iter().rev() {
        rows.sort_by(|left, right| {
            let comparison =
                compare_json(&sort_row_value(left, spec), &sort_row_value(right, spec));
            if spec.asc {
                comparison
            } else {
                comparison.reverse()
            }
        });
    }
    Ok(())
}

pub(crate) fn sort_row_value(row: &ResultRow, spec: &OrderSpec) -> Value {
    if spec.key.eq_ignore_ascii_case("_score") {
        return json!(row.score);
    }
    let value = row_value(row, &spec.key).cloned().unwrap_or(Value::Null);
    let Some(origin) = spec.geo_distance_from else {
        return value;
    };
    geo_points_from_json(&value)
        .ok()
        .and_then(|points| distance_for_points(&points, origin, spec.geo_distance_mode))
        .and_then(Number::from_f64)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

pub(crate) fn geo_points_from_json(value: &Value) -> Result<Vec<GeoPoint>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if value.is_array() {
        crate::geo::parse_geo_json(value, true)
    } else {
        crate::geo::parse_geo_json(value, false)
    }
}

pub(crate) fn sort_projected(
    rows: &mut [Vec<Value>],
    columns: &[String],
    order: &[OrderSpec],
) -> Result<()> {
    let specs = order
        .iter()
        .map(|spec| {
            let index = columns
                .iter()
                .position(|name| name.eq_ignore_ascii_case(&spec.key))
                .ok_or_else(|| anyhow!("sort key must be projected: {}", spec.key))?;
            Ok((index, spec.asc))
        })
        .collect::<Result<Vec<_>>>()?;
    for (index, ascending) in specs.into_iter().rev() {
        rows.sort_by(|left, right| {
            let comparison = compare_json(&left[index], &right[index]);
            if ascending {
                comparison
            } else {
                comparison.reverse()
            }
        });
    }
    Ok(())
}

fn compare_json(left: &Value, right: &Value) -> std::cmp::Ordering {
    match (left, right) {
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Greater,
        (_, Value::Null) => std::cmp::Ordering::Less,
        (Value::Number(left), Value::Number(right)) => left
            .as_f64()
            .partial_cmp(&right.as_f64())
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(left), Value::String(right)) => left.cmp(right),
        (Value::Bool(left), Value::Bool(right)) => left.cmp(right),
        _ => left.to_string().cmp(&right.to_string()),
    }
}

pub(crate) struct NativeSort {
    pub(super) fields: Vec<NativeSortField>,
}

#[derive(Clone)]
pub(crate) struct NativeSortField {
    pub(super) field: String,
    pub(super) data_type: ColumnType,
    pub(super) order: Order,
    pub(super) geo_distance_from: Option<GeoPoint>,
    pub(super) geo_distance_mode: GeoDistanceMode,
}

pub(crate) fn collect_native_sorted_docs(
    searcher: &Searcher,
    query: &dyn Query,
    sort: &NativeSort,
    limit: usize,
    offset: usize,
    pool: &rayon::ThreadPool,
) -> Result<Vec<(f32, DocAddress)>> {
    if block_top_k_supported(sort) {
        return collect_block_top_k(searcher, query, sort, limit, offset, pool);
    }
    let collector = TopDocs::with_limit(limit)
        .and_offset(offset)
        .order_by(DynamicSortKeyComputer::new(sort.fields.clone()));
    Ok(searcher
        .search(query, &collector)?
        .into_iter()
        .map(|(_, address)| (0.0, address))
        .collect())
}

pub(crate) fn block_top_k_supported(sort: &NativeSort) -> bool {
    (1..=2).contains(&sort.fields.len())
        && sort.fields.iter().all(|field| {
            field.geo_distance_from.is_none()
                && matches!(
                    field.data_type,
                    ColumnType::Integer
                        | ColumnType::Unsigned
                        | ColumnType::Real
                        | ColumnType::Boolean
                        | ColumnType::Date
                        | ColumnType::DateTime
                        | ColumnType::Timestamp
                )
        })
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DynamicSortComparator {
    orders: Vec<ComparatorEnum>,
}

#[derive(Clone, Debug)]
pub(crate) enum DynamicSortKey<T> {
    One(T),
    Two(T, T),
    Many(Vec<T>),
}

impl<T> Comparator<DynamicSortKey<T>> for DynamicSortComparator
where
    T: std::fmt::Debug + Send + Sync,
    ComparatorEnum: Comparator<T>,
{
    fn compare(&self, left: &DynamicSortKey<T>, right: &DynamicSortKey<T>) -> std::cmp::Ordering {
        match (left, right) {
            (DynamicSortKey::One(left), DynamicSortKey::One(right)) => {
                Comparator::compare(&self.orders[0], left, right)
            }
            (
                DynamicSortKey::Two(left_first, left_second),
                DynamicSortKey::Two(right_first, right_second),
            ) => Comparator::compare(&self.orders[0], left_first, right_first)
                .then_with(|| Comparator::compare(&self.orders[1], left_second, right_second)),
            (DynamicSortKey::Many(left), DynamicSortKey::Many(right)) => {
                compare_sort_keys(&self.orders, left, right)
            }
            _ => unreachable!("sort keys from one collector have the same shape"),
        }
    }
}

fn compare_sort_keys<T>(orders: &[ComparatorEnum], left: &[T], right: &[T]) -> std::cmp::Ordering
where
    ComparatorEnum: Comparator<T>,
{
    orders
        .iter()
        .zip(left.iter().zip(right))
        .map(|(order, (left, right))| Comparator::compare(order, left, right))
        .find(|ordering| !ordering.is_eq())
        .unwrap_or(std::cmp::Ordering::Equal)
}

pub(crate) struct DynamicSortKeyComputer {
    fields: Vec<NativeSortField>,
    comparator: DynamicSortComparator,
}

impl DynamicSortKeyComputer {
    fn new(fields: Vec<NativeSortField>) -> Self {
        let comparator = DynamicSortComparator {
            orders: fields
                .iter()
                .map(|field| match field.order {
                    Order::Asc => ComparatorEnum::ReverseNoneLower,
                    // The materialized sort puts nulls first for descending order. Tantivy's
                    // default descending comparator puts them last, so select its explicit
                    // null-high variant to keep both execution paths equivalent.
                    Order::Desc => ComparatorEnum::NaturalNoneHigher,
                })
                .collect(),
        };
        Self { fields, comparator }
    }
}

impl SortKeyComputer for DynamicSortKeyComputer {
    type SortKey = DynamicSortKey<OwnedValue>;
    type Child = DynamicSegmentSortKeyComputer;
    type Comparator = DynamicSortComparator;

    fn comparator(&self) -> Self::Comparator {
        self.comparator.clone()
    }

    fn segment_sort_key_computer(&self, segment: &SegmentReader) -> tantivy::Result<Self::Child> {
        let fast_fields = segment.fast_fields();
        let mut readers = Vec::with_capacity(self.fields.len());
        for field in &self.fields {
            readers.push(sort_fast_values(fast_fields, field)?);
        }
        Ok(DynamicSegmentSortKeyComputer {
            readers,
            comparator: self.comparator.clone(),
        })
    }
}

fn sort_fast_values(
    fast_fields: &tantivy::fastfield::FastFieldReaders,
    field: &NativeSortField,
) -> tantivy::Result<FastValues> {
    if let Some(origin) = field.geo_distance_from {
        return Ok(FastValues::GeoDistance {
            coordinates: fast_fields
                .bytes(&geo_coordinate_field(&field.field))?
                .ok_or_else(|| missing_fast_field(&field.field))?,
            origin,
            mode: field.geo_distance_mode,
        });
    }
    Ok(match field.data_type {
        ColumnType::Integer => FastValues::Integer(fast_fields.i64(&field.field)?),
        ColumnType::Unsigned => FastValues::Unsigned(fast_fields.u64(&field.field)?),
        ColumnType::Real => FastValues::Real(fast_fields.f64(&field.field)?),
        ColumnType::Text => FastValues::Text(
            fast_fields
                .str(&field.field)?
                .ok_or_else(|| missing_fast_field(&field.field))?,
        ),
        ColumnType::Boolean => FastValues::Boolean(fast_fields.bool(&field.field)?),
        ColumnType::Date | ColumnType::DateTime | ColumnType::Timestamp => {
            FastValues::Date(fast_fields.date(&field.field)?)
        }
        ColumnType::Ip => FastValues::Ip(fast_fields.ip_addr(&field.field)?),
        ColumnType::Blob => FastValues::Blob(
            fast_fields
                .bytes(&field.field)?
                .ok_or_else(|| missing_fast_field(&field.field))?,
        ),
        ColumnType::GeoPoint | ColumnType::GeoPointArray => {
            return Err(tantivy::TantivyError::SchemaError(
                "geo sort requires geo_distance_from".into(),
            ));
        }
        data_type
            if data_type.is_array()
                || matches!(data_type, ColumnType::Json | ColumnType::Facet) =>
        {
            return Err(tantivy::TantivyError::SchemaError(
                "array columns cannot be sorted".into(),
            ));
        }
        _ => {
            return Err(tantivy::TantivyError::SchemaError(
                "unsupported native sort type".into(),
            ));
        }
    })
}

fn missing_fast_field(field: &str) -> tantivy::TantivyError {
    tantivy::TantivyError::SchemaError(format!("missing fast field: {field}"))
}

pub(crate) struct DynamicSegmentSortKeyComputer {
    readers: Vec<FastValues>,
    comparator: DynamicSortComparator,
}

impl SegmentSortKeyComputer for DynamicSegmentSortKeyComputer {
    type SortKey = DynamicSortKey<OwnedValue>;
    type SegmentSortKey = DynamicSortKey<Option<u64>>;
    type SegmentComparator = DynamicSortComparator;

    fn segment_comparator(&self) -> Self::SegmentComparator {
        self.comparator.clone()
    }

    fn segment_sort_key(&mut self, doc: DocId, _score: Score) -> Self::SegmentSortKey {
        match self.readers.as_slice() {
            [reader] => DynamicSortKey::One(reader.segment_sort_value(doc)),
            [first, second] => DynamicSortKey::Two(
                first.segment_sort_value(doc),
                second.segment_sort_value(doc),
            ),
            readers => DynamicSortKey::Many(
                readers
                    .iter()
                    .map(|reader| reader.segment_sort_value(doc))
                    .collect(),
            ),
        }
    }

    fn convert_segment_sort_key(&self, key: Self::SegmentSortKey) -> Self::SortKey {
        match key {
            DynamicSortKey::One(value) => {
                DynamicSortKey::One(self.readers[0].global_sort_value(value))
            }
            DynamicSortKey::Two(first, second) => DynamicSortKey::Two(
                self.readers[0].global_sort_value(first),
                self.readers[1].global_sort_value(second),
            ),
            DynamicSortKey::Many(values) => DynamicSortKey::Many(
                self.readers
                    .iter()
                    .zip(values)
                    .map(|(reader, value)| reader.global_sort_value(value))
                    .collect(),
            ),
        }
    }
}
