use super::*;

/// Adds the primary key as a deterministic tie-breaker to explicit sort orders.
pub(crate) fn stable_typed_order(def: &TableDef, request: &ReadRequest) -> Vec<OrderSpec> {
    let mut order = request
        .order_by
        .iter()
        .map(|sort| OrderSpec {
            key: sort
                .json_path
                .as_ref()
                .map(|path| format!("{}.{}", sort.column, path))
                .unwrap_or_else(|| sort.column.clone()),
            json_type: sort.json_type,
            asc: !sort.descending,
        })
        .collect::<Vec<_>>();
    if order.is_empty()
        || typed_is_aggregation(request)
        || order
            .iter()
            .any(|spec| spec.key.eq_ignore_ascii_case("_score"))
    {
        return order;
    }
    let primary_key = &def.columns[primary_key_index(def)].name;
    if !order
        .iter()
        .any(|spec| spec.key.eq_ignore_ascii_case(primary_key))
    {
        order.push(OrderSpec {
            key: primary_key.clone(),
            json_type: None,
            asc: true,
        });
    }
    order
}

/// Validates keyset pagination and combines its lexicographic boundary with the user filter.
pub(crate) fn filter_after_cursor(
    def: &TableDef,
    request: &ReadRequest,
    order: &[OrderSpec],
    native_sort: Option<&NativeSort>,
) -> Result<Option<Filter>> {
    let Some(values) = &request.search_after else {
        return Ok(request.filter.clone());
    };
    ensure!(
        request.offset == 0,
        "search_after cannot be combined with offset"
    );
    ensure!(
        !request.order_by.is_empty(),
        "search_after requires an explicit order_by"
    );
    ensure!(
        native_sort.is_some(),
        "search_after requires a non-nullable native scalar sort without scoring"
    );
    ensure!(
        order.iter().all(|spec| spec.json_type.is_none()),
        "search_after does not support dynamic JSON path sorting"
    );
    ensure!(
        values.len() == order.len(),
        "search_after requires {} sort value(s), including the primary-key tie-breaker",
        order.len()
    );

    let mut alternatives = Vec::with_capacity(order.len());
    for index in 0..order.len() {
        let mut filters = Vec::with_capacity(index + 1);
        for previous in 0..index {
            filters.push(Filter::Compare {
                column: order[previous].key.clone(),
                operator: Comparison::Equal,
                value: values[previous].clone(),
            });
        }
        let spec = &order[index];
        let column = column(def, &spec.key)?;
        ensure!(
            column.index.indexed,
            "cursor column is not indexed: {}",
            spec.key
        );
        filters.push(Filter::Compare {
            column: spec.key.clone(),
            operator: if spec.asc {
                Comparison::Greater
            } else {
                Comparison::Less
            },
            value: values[index].clone(),
        });
        alternatives.push(if filters.len() == 1 {
            filters.pop().expect("one cursor filter")
        } else {
            Filter::All { filters }
        });
    }
    let cursor = Filter::Any {
        filters: alternatives,
    };
    Ok(Some(match &request.filter {
        Some(filter) => Filter::All {
            filters: vec![filter.clone(), cursor],
        },
        None => cursor,
    }))
}

pub(crate) fn cursor_pagination_enabled(
    def: &TableDef,
    request: &ReadRequest,
    order: &[OrderSpec],
    native_sort: Option<&NativeSort>,
) -> bool {
    !request.order_by.is_empty()
        && native_sort.is_some()
        && order.iter().all(|spec| {
            spec.json_type.is_none()
                && column(def, &spec.key).is_ok_and(|column| column.index.indexed)
        })
}

pub(crate) fn cursor_values(row: &ResultRow, order: &[OrderSpec]) -> Result<Vec<Value>> {
    order
        .iter()
        .map(|spec| {
            row_value(row, &spec.key)
                .cloned()
                .ok_or_else(|| anyhow!("cursor sort value was not loaded: {}", spec.key))
        })
        .collect()
}
