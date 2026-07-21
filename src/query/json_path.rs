use super::*;

pub(crate) fn json_path_field(def: &TableDef, column_name: &str, path: &str) -> Result<String> {
    let column = column(def, column_name)?;
    ensure!(
        matches!(column.data_type, ColumnType::Json | ColumnType::JsonArray),
        "JSON path requires a JSON or JSON[] column"
    );
    ensure!(!path.trim().is_empty(), "JSON path cannot be empty");
    Ok(format!("{}.{}", column.name, path))
}

pub(crate) fn json_path_term(
    index: &Index,
    def: &TableDef,
    column_name: &str,
    path: &str,
    data_type: JsonPathType,
    value: &Value,
) -> Result<Term> {
    json_path_term_for_schema(&index.schema(), def, column_name, path, data_type, value)
}

pub(crate) fn json_path_term_for_schema(
    schema: &tantivy::schema::Schema,
    def: &TableDef,
    column_name: &str,
    path: &str,
    data_type: JsonPathType,
    value: &Value,
) -> Result<Term> {
    let column = column(def, column_name)?;
    ensure!(
        column.index.indexed,
        "JSON column is not indexed: {column_name}"
    );
    json_path_field(def, column_name, path)?;
    let field = schema.get_field(&column.name)?;
    let mut term = Term::from_field_json_path(field, path, false);
    match data_type {
        JsonPathType::String => term.append_type_and_str(
            value
                .as_str()
                .context("JSON string path value must be a string")?,
        ),
        JsonPathType::I64 => term.append_type_and_fast_value(
            value
                .as_i64()
                .context("JSON i64 path value must be an integer")?,
        ),
        JsonPathType::U64 => term.append_type_and_fast_value(
            value
                .as_u64()
                .context("JSON u64 path value must be an unsigned integer")?,
        ),
        JsonPathType::F64 => {
            let value = value
                .as_f64()
                .context("JSON f64 path value must be numeric")?;
            ensure!(value.is_finite(), "JSON f64 path value must be finite");
            term.append_type_and_fast_value(value);
        }
        JsonPathType::Bool => term.append_type_and_fast_value(
            value
                .as_bool()
                .context("JSON bool path value must be boolean")?,
        ),
        JsonPathType::DateTime => {
            let value = ChronoDateTime::parse_from_rfc3339(
                value
                    .as_str()
                    .context("JSON datetime path value must be an RFC 3339 string")?,
            )?;
            term.append_type_and_fast_value(DateTime::from_timestamp_micros(
                value.timestamp_micros(),
            ));
        }
    }
    Ok(term)
}

pub(crate) fn json_path_column_type(data_type: JsonPathType) -> ColumnType {
    match data_type {
        JsonPathType::String => ColumnType::Text,
        JsonPathType::I64 => ColumnType::Integer,
        JsonPathType::U64 => ColumnType::Unsigned,
        JsonPathType::F64 => ColumnType::Real,
        JsonPathType::Bool => ColumnType::Boolean,
        JsonPathType::DateTime => ColumnType::DateTime,
    }
}

pub(crate) fn validate_json_read_paths(
    searcher: &Searcher,
    def: &TableDef,
    request: &ReadRequest,
) -> Result<()> {
    validate_filter_paths(searcher, def, request.filter.as_ref())?;
    for sort in &request.order_by {
        match (&sort.json_path, sort.json_type) {
            (None, None) => {}
            (Some(path), Some(data_type)) => {
                validate_observed_type(searcher, def, &sort.column, path, data_type)?;
            }
            _ => bail!("JSON sort requires both json_path and json_type"),
        }
    }
    Ok(())
}

pub(crate) fn validate_json_aggregation_paths(
    searcher: &Searcher,
    def: &TableDef,
    aggregations: &BTreeMap<String, Aggregation>,
) -> Result<()> {
    for aggregation in aggregations.values() {
        validate_aggregation_path(searcher, def, aggregation)?;
    }
    Ok(())
}

pub(crate) fn validate_filter_only_json_paths(
    searcher: &Searcher,
    def: &TableDef,
    filter: Option<&Filter>,
) -> Result<()> {
    validate_filter_paths(searcher, def, filter)
}

fn validate_filter_paths(
    searcher: &Searcher,
    def: &TableDef,
    filter: Option<&Filter>,
) -> Result<()> {
    let Some(filter) = filter else { return Ok(()) };
    match filter {
        Filter::JsonCompare {
            column,
            path,
            data_type,
            ..
        }
        | Filter::JsonBetween {
            column,
            path,
            data_type,
            ..
        } => validate_observed_type(searcher, def, column, path, *data_type),
        Filter::JsonExists {
            column,
            path,
            data_type: Some(data_type),
            ..
        } => validate_observed_type(searcher, def, column, path, *data_type),
        Filter::JsonExists {
            column,
            path,
            data_type: None,
            ..
        } => {
            json_path_field(def, column, path)?;
            Ok(())
        }
        Filter::All { filters } | Filter::Any { filters } => {
            for filter in filters {
                validate_filter_paths(searcher, def, Some(filter))?;
            }
            Ok(())
        }
        Filter::Not { filter } => validate_filter_paths(searcher, def, Some(filter)),
        _ => Ok(()),
    }
}

fn validate_aggregation_path(
    searcher: &Searcher,
    def: &TableDef,
    aggregation: &Aggregation,
) -> Result<()> {
    if let Aggregation::Filter { filter, .. } = aggregation {
        validate_filter_paths(searcher, def, Some(filter))?;
    }
    let (target, children) = match aggregation {
        Aggregation::JsonTerms {
            target,
            aggregations,
            ..
        }
        | Aggregation::JsonHistogram {
            target,
            aggregations,
            ..
        }
        | Aggregation::JsonRange {
            target,
            aggregations,
            ..
        } => (Some(target), Some(aggregations)),
        Aggregation::Metric {
            json_path: Some(target),
            ..
        } => (Some(target), None),
        Aggregation::Terms { aggregations, .. }
        | Aggregation::Histogram { aggregations, .. }
        | Aggregation::DateHistogram { aggregations, .. }
        | Aggregation::Range { aggregations, .. }
        | Aggregation::Filter { aggregations, .. }
        | Aggregation::Composite { aggregations, .. } => (None, Some(aggregations)),
        Aggregation::Metric { .. }
        | Aggregation::TopHits { .. }
        | Aggregation::GeoTileGrid { .. } => (None, None),
    };
    if let Some(target) = target {
        validate_observed_type(
            searcher,
            def,
            &target.column,
            &target.path,
            target.data_type,
        )?;
    }
    if let Aggregation::Composite { sources, .. } = aggregation {
        for source in sources {
            if let CompositeSource::JsonTerms { target, .. } = source {
                validate_observed_type(
                    searcher,
                    def,
                    &target.column,
                    &target.path,
                    target.data_type,
                )?;
            }
        }
    }
    if let Aggregation::TopHits { sort, .. } = aggregation {
        for sort in sort {
            match (&sort.json_path, sort.json_type) {
                (None, None) => {}
                (Some(path), Some(data_type)) => {
                    validate_observed_type(searcher, def, &sort.column, path, data_type)?;
                }
                _ => bail!("JSON sort requires both json_path and json_type"),
            }
        }
    }
    if let Some(children) = children {
        validate_json_aggregation_paths(searcher, def, children)?;
    }
    Ok(())
}

fn validate_observed_type(
    searcher: &Searcher,
    def: &TableDef,
    column: &str,
    path: &str,
    expected: JsonPathType,
) -> Result<()> {
    let full_path = json_path_field(def, column, path)?;
    let mut observed = std::collections::BTreeSet::new();
    for segment in searcher.segment_readers() {
        for handle in segment.fast_fields().dynamic_column_handles(&full_path)? {
            observed.insert(handle.column_type());
        }
    }
    ensure!(
        !observed.is_empty(),
        "JSON path does not exist: {full_path}"
    );
    let expected = dynamic_type(expected);
    ensure!(
        observed.len() == 1 && observed.contains(&expected),
        "JSON path {full_path} has dynamic type(s) {:?}, expected {expected}",
        observed
    );
    Ok(())
}

fn dynamic_type(data_type: JsonPathType) -> DynamicColumnType {
    match data_type {
        JsonPathType::String => DynamicColumnType::Str,
        JsonPathType::I64 => DynamicColumnType::I64,
        JsonPathType::U64 => DynamicColumnType::U64,
        JsonPathType::F64 => DynamicColumnType::F64,
        JsonPathType::Bool => DynamicColumnType::Bool,
        JsonPathType::DateTime => DynamicColumnType::DateTime,
    }
}
