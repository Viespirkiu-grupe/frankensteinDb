use super::*;

/// Compiles the public filter model directly to Tantivy without a query language parser.
pub(crate) fn compile_filter(
    index: &Index,
    def: &TableDef,
    fields: &IndexFields,
    filter: Option<&Filter>,
) -> Result<QueryPlan> {
    let Some(filter) = filter else {
        return Ok(QueryPlan {
            query: Box::new(AllQuery),
        });
    };
    Ok(QueryPlan {
        query: compile_filter_node(index, def, fields, filter)?,
    })
}

fn compile_filter_node(
    index: &Index,
    def: &TableDef,
    fields: &IndexFields,
    filter: &Filter,
) -> Result<Box<dyn Query>> {
    match filter {
        Filter::Compare {
            column: name,
            operator,
            value,
        } => {
            queryable_column(def, name)?;
            constant_filter(compile_typed_comparison(
                def, fields, name, *operator, value,
            ))
        }
        Filter::Between {
            column: name,
            lower,
            upper,
        } => {
            let column = queryable_column(def, name)?;
            constant_filter(Ok(Box::new(RangeQuery::new(
                Bound::Included(term_for_value(fields, column, lower)?),
                Bound::Included(term_for_value(fields, column, upper)?),
            ))))
        }
        Filter::In {
            column: name,
            values,
        } => {
            ensure!(!values.is_empty(), "IN filter requires at least one value");
            let column = queryable_column(def, name)?;
            let terms = values
                .iter()
                .map(|value| term_for_value(fields, column, value))
                .collect::<Result<Vec<_>>>()?;
            constant_filter(Ok(Box::new(TermSetQuery::new(terms))))
        }
        Filter::IsNull {
            column: name,
            negated,
        } => {
            let query: Box<dyn Query> = Box::new(ExistsQuery::new(
                existence_field(queryable_column(def, name)?),
                false,
            ));
            constant_filter(if *negated { Ok(query) } else { negate(query) })
        }
        Filter::Search {
            fields: names,
            query,
        } => compile_typed_search(index, def, names, query),
        Filter::SearchBoosted {
            fields: boosts,
            query,
            conjunction_by_default,
        } => compile_boosted_search(index, def, boosts, query, *conjunction_by_default),
        Filter::Fuzzy {
            column: name,
            value,
            distance,
            transposition_cost_one,
        } => compile_fuzzy(
            index,
            def,
            name,
            value,
            *distance,
            *transposition_cost_one,
            false,
        ),
        Filter::Prefix {
            column: name,
            value,
        } => compile_fuzzy(index, def, name, value, 0, false, true),
        Filter::PhrasePrefix {
            column: name,
            phrase,
            max_expansions,
        } => compile_phrase_prefix(index, def, name, phrase, *max_expansions),
        Filter::DisjunctionMax {
            fields,
            query,
            tie_breaker,
        } => compile_disjunction_max(index, def, fields, query, *tie_breaker),
        Filter::Regex {
            column: name,
            pattern,
        } => compile_regex(index, def, name, pattern),
        Filter::RegexPhrase {
            column: name,
            patterns,
            slop,
            max_expansions,
        } => compile_regex_phrase(index, def, name, patterns, *slop, *max_expansions),
        Filter::JsonSearch {
            column: name,
            path,
            query,
        } => {
            let column = column(def, name)?;
            ensure!(
                matches!(column.data_type, ColumnType::Json | ColumnType::JsonArray),
                "json_search requires JSON or JSON[] column"
            );
            ensure!(column.index.indexed, "column is not indexed: {name}");
            ensure!(
                !path.is_empty() && !query.trim().is_empty(),
                "JSON path and query are required"
            );
            let field = index.schema().get_field(&column.name)?;
            Ok(
                QueryParser::for_index(index, vec![field]).parse_query(&format!(
                    "{}:({})",
                    path.replace('\\', "\\\\").replace(':', "\\:"),
                    query
                ))?,
            )
        }
        Filter::JsonCompare {
            column,
            path,
            data_type,
            operator,
            value,
        } => constant_filter(compile_json_comparison(
            index, def, column, path, *data_type, *operator, value,
        )),
        Filter::JsonBetween {
            column,
            path,
            data_type,
            lower,
            upper,
        } => constant_filter(Ok(Box::new(RangeQuery::new(
            Bound::Included(json_path_term(index, def, column, path, *data_type, lower)?),
            Bound::Included(json_path_term(index, def, column, path, *data_type, upper)?),
        )))),
        Filter::JsonExists {
            column,
            path,
            negated,
            ..
        } => {
            let query: Box<dyn Query> =
                Box::new(ExistsQuery::new(json_path_field(def, column, path)?, false));
            constant_filter(if *negated { negate(query) } else { Ok(query) })
        }
        Filter::All { filters } => compile_boolean(index, def, fields, filters, Occur::Must),
        Filter::Any { filters } => compile_boolean(index, def, fields, filters, Occur::Should),
        Filter::Not { filter } => {
            constant_filter(negate(compile_filter_node(index, def, fields, filter)?))
        }
    }
}

fn compile_json_comparison(
    index: &Index,
    def: &TableDef,
    column: &str,
    path: &str,
    data_type: JsonPathType,
    operator: Comparison,
    value: &Value,
) -> Result<Box<dyn Query>> {
    let term = json_path_term(index, def, column, path, data_type, value)?;
    if data_type == JsonPathType::Bool {
        ensure!(
            matches!(operator, Comparison::Equal | Comparison::NotEqual),
            "JSON bool paths support only equal and not_equal"
        );
        let query: Box<dyn Query> = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        return if operator == Comparison::NotEqual {
            negate(query)
        } else {
            Ok(query)
        };
    }
    let (lower, upper) = match operator {
        Comparison::Equal | Comparison::NotEqual => {
            (Bound::Included(term.clone()), Bound::Included(term))
        }
        Comparison::Greater => (Bound::Excluded(term), Bound::Unbounded),
        Comparison::GreaterOrEqual => (Bound::Included(term), Bound::Unbounded),
        Comparison::Less => (Bound::Unbounded, Bound::Excluded(term)),
        Comparison::LessOrEqual => (Bound::Unbounded, Bound::Included(term)),
    };
    let query: Box<dyn Query> = Box::new(RangeQuery::new(lower, upper));
    if operator == Comparison::NotEqual {
        negate(query)
    } else {
        Ok(query)
    }
}

fn constant_filter(query: Result<Box<dyn Query>>) -> Result<Box<dyn Query>> {
    Ok(Box::new(ConstScoreQuery::new(query?, 0.0)))
}

fn compile_boolean(
    index: &Index,
    def: &TableDef,
    fields: &IndexFields,
    filters: &[Filter],
    occur: Occur,
) -> Result<Box<dyn Query>> {
    ensure!(
        !filters.is_empty(),
        "boolean filter requires at least one child"
    );
    let clauses = filters
        .iter()
        .map(|filter| Ok((occur, compile_filter_node(index, def, fields, filter)?)))
        .collect::<Result<Vec<_>>>()?;
    Ok(Box::new(BooleanQuery::new(clauses)))
}

fn compile_typed_search(
    index: &Index,
    def: &TableDef,
    names: &[String],
    query: &str,
) -> Result<Box<dyn Query>> {
    ensure!(!query.trim().is_empty(), "search query cannot be empty");
    let names = if names.is_empty() {
        def.columns
            .iter()
            .filter(|column| {
                column.index.indexed
                    && matches!(column.data_type, ColumnType::Text | ColumnType::TextArray)
            })
            .map(|column| column.name.clone())
            .collect::<Vec<_>>()
    } else {
        names.to_vec()
    };
    let search_fields = names
        .iter()
        .map(|name| {
            let column = searchable_column(def, name)?;
            Ok(index.schema().get_field(&column.name)?)
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!search_fields.is_empty(), "table has no searchable columns");
    Ok(QueryParser::for_index(index, search_fields).parse_query(query)?)
}

fn compile_boosted_search(
    index: &Index,
    def: &TableDef,
    boosts: &BTreeMap<String, f32>,
    query: &str,
    conjunction_by_default: bool,
) -> Result<Box<dyn Query>> {
    ensure!(
        !boosts.is_empty(),
        "boosted search requires at least one field"
    );
    let mut fields = Vec::with_capacity(boosts.len());
    for (name, boost) in boosts {
        ensure!(
            boost.is_finite() && *boost > 0.0,
            "field boost must be positive"
        );
        let column = searchable_column(def, name)?;
        fields.push((index.schema().get_field(&column.name)?, *boost));
    }
    let mut parser =
        QueryParser::for_index(index, fields.iter().map(|(field, _)| *field).collect());
    if conjunction_by_default {
        parser.set_conjunction_by_default();
    }
    for (field, boost) in fields {
        parser.set_field_boost(field, boost);
    }
    Ok(parser.parse_query(query)?)
}

fn compile_fuzzy(
    index: &Index,
    def: &TableDef,
    name: &str,
    value: &str,
    distance: u8,
    transposition_cost_one: bool,
    prefix: bool,
) -> Result<Box<dyn Query>> {
    ensure!(distance <= 2, "fuzzy distance cannot exceed 2");
    ensure!(!value.trim().is_empty(), "search value cannot be empty");
    let column = searchable_column(def, name)?;
    let field = index.schema().get_field(&column.name)?;
    let mut analyzer = index
        .tokenizers()
        .get(&analyzer_name(column))
        .ok_or_else(|| anyhow!("missing analyzer for {name}"))?;
    let mut stream = analyzer.token_stream(value);
    ensure!(stream.advance(), "search value produced no token");
    let token = stream.token().text.clone();
    ensure!(
        !stream.advance(),
        "fuzzy/prefix search accepts one analyzed token"
    );
    let term = Term::from_field_text(field, &token);
    Ok(Box::new(if prefix {
        FuzzyTermQuery::new_prefix(term, distance, transposition_cost_one)
    } else {
        FuzzyTermQuery::new(term, distance, transposition_cost_one)
    }))
}

fn compile_regex_phrase(
    index: &Index,
    def: &TableDef,
    name: &str,
    patterns: &[String],
    slop: u32,
    max_expansions: u32,
) -> Result<Box<dyn Query>> {
    ensure!(
        (2..=16).contains(&patterns.len()),
        "regex_phrase requires 2..=16 patterns"
    );
    ensure!(slop <= 64, "regex_phrase slop cannot exceed 64");
    ensure!(
        (1..=16_384).contains(&max_expansions),
        "regex_phrase max_expansions must be 1..=16384"
    );
    ensure!(
        patterns
            .iter()
            .all(|pattern| !pattern.is_empty() && pattern.len() <= 512),
        "regex_phrase patterns must contain 1..=512 bytes"
    );
    let column = searchable_column(def, name)?;
    ensure!(
        column.index.record == TextIndexRecord::Positions,
        "regex_phrase requires positions for column: {name}"
    );
    let field = index.schema().get_field(&column.name)?;
    let mut query = RegexPhraseQuery::new(field, patterns.to_vec());
    query.set_slop(slop);
    query.set_max_expansions(max_expansions);
    Ok(Box::new(query))
}

fn compile_regex(
    index: &Index,
    def: &TableDef,
    name: &str,
    pattern: &str,
) -> Result<Box<dyn Query>> {
    ensure!(
        !pattern.is_empty() && pattern.len() <= 512,
        "regex pattern must contain 1..=512 bytes"
    );
    let column = searchable_column(def, name)?;
    let field = index.schema().get_field(&column.name)?;
    Ok(Box::new(RegexQuery::from_pattern(pattern, field)?))
}

pub(crate) fn searchable_column<'a>(def: &'a TableDef, name: &str) -> Result<&'a ColumnDef> {
    let column = column(def, name)?;
    ensure!(
        matches!(column.data_type, ColumnType::Text | ColumnType::TextArray),
        "search field must be TEXT or TEXT[]: {name}"
    );
    ensure!(column.index.indexed, "column is not indexed: {name}");
    Ok(column)
}

fn queryable_column<'a>(def: &'a TableDef, name: &str) -> Result<&'a ColumnDef> {
    let column = column(def, name)?;
    ensure!(column.index.indexed, "column is not indexed: {name}");
    Ok(column)
}

fn compile_typed_comparison(
    def: &TableDef,
    fields: &IndexFields,
    name: &str,
    operator: Comparison,
    value: &Value,
) -> Result<Box<dyn Query>> {
    let term = term_for_value(fields, column(def, name)?, value)?;
    match operator {
        Comparison::Equal => Ok(term_query(term)),
        Comparison::NotEqual => negate(term_query(term)),
        Comparison::Greater => Ok(Box::new(RangeQuery::new(
            Bound::Excluded(term),
            Bound::Unbounded,
        ))),
        Comparison::GreaterOrEqual => Ok(Box::new(RangeQuery::new(
            Bound::Included(term),
            Bound::Unbounded,
        ))),
        Comparison::Less => Ok(Box::new(RangeQuery::new(
            Bound::Unbounded,
            Bound::Excluded(term),
        ))),
        Comparison::LessOrEqual => Ok(Box::new(RangeQuery::new(
            Bound::Unbounded,
            Bound::Included(term),
        ))),
    }
}

pub(crate) fn term_for_value(
    fields: &IndexFields,
    column: &ColumnDef,
    value: &Value,
) -> Result<Term> {
    let field = if matches!(column.data_type, ColumnType::Text | ColumnType::TextArray) {
        fields.raw[&column.name]
    } else {
        fields.values[&column.name]
    };
    Ok(match (&column.data_type, value) {
        (ColumnType::Integer | ColumnType::IntegerArray, Value::Number(value)) => {
            Term::from_field_i64(field, value.as_i64().context("expected integer")?)
        }
        (ColumnType::Unsigned | ColumnType::UnsignedArray, Value::Number(value)) => {
            Term::from_field_u64(field, value.as_u64().context("expected unsigned integer")?)
        }
        (ColumnType::Real | ColumnType::RealArray, Value::Number(value)) => {
            Term::from_field_f64(field, value.as_f64().context("expected number")?)
        }
        (ColumnType::Boolean | ColumnType::BooleanArray, Value::Bool(value)) => {
            Term::from_field_bool(field, *value)
        }
        (ColumnType::Text | ColumnType::TextArray, Value::String(value)) => {
            Term::from_field_text(field, value)
        }
        (ColumnType::Date | ColumnType::DateArray, Value::String(value)) => {
            let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?;
            let micros = date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_micros();
            Term::from_field_date_for_search(field, DateTime::from_timestamp_micros(micros))
        }
        (ColumnType::DateTime | ColumnType::DateTimeArray, Value::String(value)) => {
            let micros = ChronoDateTime::parse_from_rfc3339(value)?.timestamp_micros();
            Term::from_field_date_for_search(field, DateTime::from_timestamp_micros(micros))
        }
        (ColumnType::Timestamp | ColumnType::TimestampArray, Value::String(value)) => {
            Term::from_field_date_for_search(field, timestamp_to_tantivy(value)?)
        }
        (ColumnType::Blob | ColumnType::BlobArray, Value::String(value)) => Term::from_field_bytes(
            field,
            &hex::decode(value.strip_prefix("0x").unwrap_or(value))?,
        ),
        (ColumnType::Ip | ColumnType::IpArray, Value::String(value)) => {
            Term::from_field_ip_addr(field, value.parse::<std::net::IpAddr>()?.into_ipv6_addr())
        }
        (ColumnType::Facet | ColumnType::FacetArray, Value::String(value)) => {
            Term::from_facet(field, &Facet::from(value.as_str()))
        }
        (ColumnType::Json | ColumnType::JsonArray, _) => {
            bail!("JSON comparison requires a full-text JSON-path query")
        }
        _ => bail!("value does not match type of column {}", column.name),
    })
}
