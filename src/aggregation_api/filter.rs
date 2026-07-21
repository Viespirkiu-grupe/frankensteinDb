use serde::{Deserialize, Serialize};
use tantivy::TantivyError;
use tantivy::aggregation::bucket::{FilterAggregation, QueryBuilder};
use tantivy::schema::Schema;
use tantivy::tokenizer::TokenizerManager;

use super::*;
use crate::sql_schema::analyzer_name;

/// Serializable bridge that keeps filter aggregations on the typed query path.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TypedFilterQueryBuilder {
    def: TableDef,
    filter: Filter,
}

pub(crate) fn typed_filter_aggregation(def: &TableDef, filter: &Filter) -> FilterAggregation {
    FilterAggregation::new_with_builder(Box::new(TypedFilterQueryBuilder {
        def: def.clone(),
        filter: filter.clone(),
    }))
}

#[typetag::serde(name = "frankensteindb_typed_filter")]
impl QueryBuilder for TypedFilterQueryBuilder {
    fn build_query(
        &self,
        schema: &Schema,
        tokenizers: &TokenizerManager,
    ) -> tantivy::Result<Box<dyn Query>> {
        compile_filter_node(schema, tokenizers, &self.def, &self.filter)
            .map_err(|error| TantivyError::InvalidArgument(error.to_string()))
    }

    fn box_clone(&self) -> Box<dyn QueryBuilder> {
        Box::new(self.clone())
    }
}

fn compile_filter_node(
    schema: &Schema,
    tokenizers: &TokenizerManager,
    def: &TableDef,
    filter: &Filter,
) -> Result<Box<dyn Query>> {
    let fields = schema_fields(schema, def)?;
    match filter {
        Filter::Compare {
            column: name,
            operator,
            value,
        } => {
            queryable_column(def, name)?;
            compile_typed_comparison(def, &fields, name, *operator, value)
        }
        Filter::Between {
            column: name,
            lower,
            upper,
        } => {
            let column = queryable_column(def, name)?;
            Ok(Box::new(RangeQuery::new(
                Bound::Included(term_for_value(&fields, column, lower)?),
                Bound::Included(term_for_value(&fields, column, upper)?),
            )))
        }
        Filter::In {
            column: name,
            values,
        } => {
            ensure!(!values.is_empty(), "IN filter requires at least one value");
            let column = queryable_column(def, name)?;
            let terms = values
                .iter()
                .map(|value| term_for_value(&fields, column, value))
                .collect::<Result<Vec<_>>>()?;
            Ok(Box::new(TermSetQuery::new(terms)))
        }
        Filter::IsNull {
            column: name,
            negated,
        } => {
            let query: Box<dyn Query> = Box::new(ExistsQuery::new(
                existence_field(queryable_column(def, name)?),
                false,
            ));
            if *negated { Ok(query) } else { negate(query) }
        }
        Filter::Search {
            fields: names,
            query,
        } => compile_search(schema, tokenizers, def, names, query),
        Filter::Fuzzy {
            column,
            value,
            distance,
            transposition_cost_one,
        } => compile_fuzzy(
            schema,
            tokenizers,
            def,
            column,
            value,
            *distance,
            *transposition_cost_one,
            false,
        ),
        Filter::Prefix { column, value } => {
            compile_fuzzy(schema, tokenizers, def, column, value, 0, false, true)
        }
        Filter::JsonCompare {
            column,
            path,
            data_type,
            operator,
            value,
        } => compile_json_comparison(schema, def, column, path, *data_type, *operator, value),
        Filter::JsonBetween {
            column,
            path,
            data_type,
            lower,
            upper,
        } => Ok(Box::new(RangeQuery::new(
            Bound::Included(json_path_term_for_schema(
                schema, def, column, path, *data_type, lower,
            )?),
            Bound::Included(json_path_term_for_schema(
                schema, def, column, path, *data_type, upper,
            )?),
        ))),
        Filter::JsonExists {
            column,
            path,
            negated,
            ..
        } => {
            let query: Box<dyn Query> =
                Box::new(ExistsQuery::new(json_path_field(def, column, path)?, false));
            if *negated { negate(query) } else { Ok(query) }
        }
        Filter::All { filters } => compile_boolean(schema, tokenizers, def, filters, Occur::Must),
        Filter::Any { filters } => compile_boolean(schema, tokenizers, def, filters, Occur::Should),
        Filter::Not { filter } => negate(compile_filter_node(schema, tokenizers, def, filter)?),
        _ => bail!("this typed filter is not supported inside a filter aggregation"),
    }
}

fn compile_boolean(
    schema: &Schema,
    tokenizers: &TokenizerManager,
    def: &TableDef,
    filters: &[Filter],
    occur: Occur,
) -> Result<Box<dyn Query>> {
    ensure!(
        !filters.is_empty(),
        "boolean filter requires at least one child"
    );
    let clauses = filters
        .iter()
        .map(|filter| Ok((occur, compile_filter_node(schema, tokenizers, def, filter)?)))
        .collect::<Result<Vec<_>>>()?;
    Ok(Box::new(BooleanQuery::new(clauses)))
}

fn compile_search(
    schema: &Schema,
    tokenizers: &TokenizerManager,
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
    let fields = names
        .iter()
        .map(|name| Ok(schema.get_field(&searchable_column(def, name)?.name)?))
        .collect::<Result<Vec<_>>>()?;
    ensure!(!fields.is_empty(), "table has no searchable columns");
    Ok(QueryParser::new(schema.clone(), fields, tokenizers.clone()).parse_query(query)?)
}

#[allow(clippy::too_many_arguments)]
fn compile_fuzzy(
    schema: &Schema,
    tokenizers: &TokenizerManager,
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
    let field = schema.get_field(&column.name)?;
    let mut analyzer = tokenizers
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

fn compile_json_comparison(
    schema: &Schema,
    def: &TableDef,
    column: &str,
    path: &str,
    data_type: JsonPathType,
    operator: Comparison,
    value: &Value,
) -> Result<Box<dyn Query>> {
    let term = json_path_term_for_schema(schema, def, column, path, data_type, value)?;
    if data_type == JsonPathType::Bool {
        ensure!(
            matches!(operator, Comparison::Equal | Comparison::NotEqual),
            "JSON bool paths support only equal and not_equal"
        );
        let query = term_query(term);
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

fn queryable_column<'a>(def: &'a TableDef, name: &str) -> Result<&'a ColumnDef> {
    let column = column(def, name)?;
    ensure!(column.index.indexed, "column is not indexed: {name}");
    Ok(column)
}
