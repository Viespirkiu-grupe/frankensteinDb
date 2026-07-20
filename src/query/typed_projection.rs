use super::*;

pub(crate) fn typed_is_aggregation(request: &ReadRequest) -> bool {
    !request.group_by.is_empty()
        || request
            .projection
            .iter()
            .any(|item| matches!(item, Projection::Aggregate { .. }))
}

pub(crate) fn typed_requires_full_scan(request: &ReadRequest, order: &[OrderSpec]) -> bool {
    typed_is_aggregation(request)
        || order
            .iter()
            .any(|spec| spec.asc || !spec.key.eq_ignore_ascii_case("_score"))
}

pub(crate) fn typed_native_sort(
    request: &ReadRequest,
    def: &TableDef,
    order: &[OrderSpec],
) -> Option<NativeSort> {
    if request.min_score.is_some()
        || order.is_empty()
        || request
            .projection
            .iter()
            .any(|item| matches!(item, Projection::Score { .. }))
    {
        return None;
    }
    let mut fields = Vec::with_capacity(order.len());
    for spec in order {
        if spec.key.eq_ignore_ascii_case("_score") {
            return None;
        }
        if let Some(data_type) = spec.json_type {
            fields.push(NativeSortField {
                field: spec.key.clone(),
                data_type: json_path_column_type(data_type),
                order: if spec.asc { Order::Asc } else { Order::Desc },
            });
            continue;
        }
        let column = column(def, &spec.key).ok()?;
        if column.nullable
            || column.data_type.is_array()
            || matches!(
                column.data_type,
                ColumnType::Ip | ColumnType::Json | ColumnType::Facet
            )
        {
            return None;
        }
        fields.push(NativeSortField {
            field: aggregation_field(column),
            data_type: column.data_type,
            order: if spec.asc { Order::Asc } else { Order::Desc },
        });
    }
    Some(NativeSort { fields })
}

pub(crate) fn required_typed_columns<'a>(
    def: &'a TableDef,
    request: &ReadRequest,
    order: &[OrderSpec],
    full_scan: bool,
) -> Result<Vec<&'a ColumnDef>> {
    let mut names = if request.projection.is_empty() {
        def.columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>()
    } else {
        request
            .projection
            .iter()
            .filter_map(|item| match item {
                Projection::Column { column, .. } => Some(column.clone()),
                Projection::Aggregate {
                    column: Some(column),
                    ..
                } => Some(column.clone()),
                Projection::Highlight { column, .. } => Some(column.clone()),
                Projection::Score { .. } | Projection::Aggregate { column: None, .. } => None,
            })
            .collect()
    };
    if full_scan {
        names.extend(order.iter().filter_map(|spec| {
            column(def, &spec.key)
                .ok()
                .map(|column| column.name.clone())
        }));
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
    names
        .iter()
        .map(|name| column(def, name))
        .collect::<Result<Vec<_>>>()
}

pub(crate) fn project_typed_rows(
    def: &TableDef,
    request: &ReadRequest,
    mut rows: Vec<ResultRow>,
    order: &[OrderSpec],
    apply_window: bool,
    cursor_mode: bool,
    highlights: &HighlightGenerators,
) -> Result<QueryResult> {
    // Bounded Tantivy collectors already return their final order. Re-sorting is only
    // necessary for the materialized full-scan path; JSON-path sort keys are not row fields.
    if apply_window {
        sort_source(&mut rows, order)?;
    }
    let projection = expanded_typed_projection(def, request)?;
    let mut source = if apply_window {
        rows.into_iter()
            .skip(request.offset)
            .take(
                request
                    .limit
                    .saturating_add(usize::from(cursor_mode && request.limit > 0)),
            )
            .collect::<Vec<_>>()
    } else {
        rows
    };
    let has_more = cursor_mode && source.len() > request.limit;
    if has_more {
        source.truncate(request.limit);
    }
    let next_search_after = if has_more {
        source
            .last()
            .map(|row| cursor_values(row, order))
            .transpose()?
    } else {
        None
    };
    let rows = source
        .iter()
        .map(|row| {
            projection
                .iter()
                .map(|item| typed_scalar_value(item, row, highlights))
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    let columns = projection
        .iter()
        .map(typed_projection_name)
        .collect::<Vec<_>>();
    Ok(QueryResult {
        message: format!("{} row(s)", rows.len()),
        columns,
        rows,
        next_search_after,
    })
}

fn expanded_typed_projection(def: &TableDef, request: &ReadRequest) -> Result<Vec<Projection>> {
    if request.projection.is_empty() {
        return Ok(def
            .columns
            .iter()
            .map(|column| Projection::Column {
                column: column.name.clone(),
                alias: None,
            })
            .collect());
    }
    for item in &request.projection {
        match item {
            Projection::Column { column: name, .. } => {
                column(def, name)?;
            }
            Projection::Score { .. } => {}
            Projection::Highlight {
                column: name,
                fragment_size,
                ..
            } => {
                let field = column(def, name)?;
                ensure!(
                    matches!(field.data_type, ColumnType::Text),
                    "highlight requires TEXT"
                );
                ensure!(
                    field.index.indexed,
                    "highlight requires an indexed TEXT column"
                );
                ensure!(
                    (32..=4096).contains(fragment_size),
                    "fragment_size must be 32..=4096"
                );
            }
            Projection::Aggregate { .. } => bail!("aggregate projection requires aggregation"),
        }
    }
    Ok(request.projection.clone())
}

pub(crate) fn typed_projection_name(item: &Projection) -> String {
    match item {
        Projection::Column { column, alias } => alias.clone().unwrap_or_else(|| column.clone()),
        Projection::Score { alias } => alias.clone().unwrap_or_else(|| "_score".into()),
        Projection::Highlight { column, alias, .. } => alias
            .clone()
            .unwrap_or_else(|| format!("{column}_highlight")),
        Projection::Aggregate { alias, .. } => alias.clone(),
    }
}

fn typed_scalar_value(
    item: &Projection,
    row: &ResultRow,
    highlights: &HighlightGenerators,
) -> Result<Value> {
    match item {
        Projection::Column { column, .. } => row_value(row, column)
            .cloned()
            .ok_or_else(|| anyhow!("unknown column: {column}")),
        Projection::Score { .. } => Ok(Number::from_f64(row.score)
            .map(Value::Number)
            .unwrap_or(Value::Null)),
        Projection::Highlight {
            column,
            fragment_size,
            ..
        } => {
            let text = row_value(row, column).and_then(Value::as_str);
            text.map(|text| {
                highlights
                    .snippet(column, *fragment_size, text)
                    .map(Value::String)
            })
            .unwrap_or(Ok(Value::Null))
        }
        Projection::Aggregate { .. } => bail!("aggregate projection requires aggregation"),
    }
}
