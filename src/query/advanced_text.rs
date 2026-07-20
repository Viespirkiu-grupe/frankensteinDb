use super::*;

pub(crate) fn compile_phrase_prefix(
    index: &Index,
    def: &TableDef,
    name: &str,
    phrase: &str,
    max_expansions: u32,
) -> Result<Box<dyn Query>> {
    ensure!(
        !phrase.trim().is_empty() && phrase.len() <= 1_024,
        "phrase_prefix phrase must contain 1..=1024 bytes"
    );
    ensure!(
        (1..=4_096).contains(&max_expansions),
        "phrase_prefix max_expansions must be 1..=4096"
    );
    let column = searchable_column(def, name)?;
    ensure!(
        column.index.record == TextIndexRecord::Positions,
        "phrase_prefix requires positions for column: {name}"
    );
    let field = index.schema().get_field(&column.name)?;
    let mut analyzer = index
        .tokenizers()
        .get(&analyzer_name(column))
        .ok_or_else(|| anyhow!("missing analyzer for {name}"))?;
    let mut stream = analyzer.token_stream(phrase);
    let mut terms = Vec::new();
    while stream.advance() {
        terms.push(Term::from_field_text(field, &stream.token().text));
    }
    ensure!(
        (2..=32).contains(&terms.len()),
        "phrase_prefix requires 2..=32 analyzed tokens"
    );
    let mut query = PhrasePrefixQuery::new(terms);
    query.set_max_expansions(max_expansions);
    Ok(Box::new(query))
}

pub(crate) fn compile_disjunction_max(
    index: &Index,
    def: &TableDef,
    fields: &BTreeMap<String, f32>,
    query: &str,
    tie_breaker: f32,
) -> Result<Box<dyn Query>> {
    ensure!(
        (1..=32).contains(&fields.len()),
        "disjunction_max requires 1..=32 fields"
    );
    ensure!(!query.trim().is_empty(), "search query cannot be empty");
    ensure!(
        tie_breaker.is_finite() && (0.0..=1.0).contains(&tie_breaker),
        "disjunction_max tie_breaker must be 0..=1"
    );
    let disjuncts = fields
        .iter()
        .map(|(name, boost)| {
            ensure!(
                boost.is_finite() && *boost > 0.0,
                "field boost must be positive"
            );
            let column = searchable_column(def, name)?;
            let field = index.schema().get_field(&column.name)?;
            let parsed = QueryParser::for_index(index, vec![field]).parse_query(query)?;
            let boosted: Box<dyn Query> = Box::new(BoostQuery::new(parsed, *boost));
            Ok(Box::new(OpaqueScoreQuery::new(boosted)) as Box<dyn Query>)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Box::new(DisjunctionMaxQuery::with_tie_breaker(
        disjuncts,
        tie_breaker,
    )))
}
