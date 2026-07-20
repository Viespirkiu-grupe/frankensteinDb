use super::*;

pub(crate) struct HighlightGenerators {
    generators: HashMap<(String, usize), SnippetGenerator>,
}

impl HighlightGenerators {
    /// Builds one tokenizer-aware Tantivy generator for each requested field and fragment size.
    pub(crate) fn create(
        searcher: &Searcher,
        schema: &tantivy::schema::Schema,
        def: &TableDef,
        query: &dyn Query,
        request: &ReadRequest,
    ) -> Result<Self> {
        let mut generators = HashMap::new();
        for projection in &request.projection {
            let Projection::Highlight {
                column: projection_column,
                fragment_size,
                ..
            } = projection
            else {
                continue;
            };
            let key = (projection_column.to_ascii_lowercase(), *fragment_size);
            if generators.contains_key(&key) {
                continue;
            }
            let canonical = column(def, projection_column)?;
            let field = schema.get_field(&canonical.name)?;
            let mut generator = SnippetGenerator::create(searcher, query, field)?;
            generator.set_max_num_chars(*fragment_size);
            generators.insert(key, generator);
        }
        Ok(Self { generators })
    }

    /// Produces escaped HTML with Tantivy-selected fragments and `<b>` match ranges.
    pub(crate) fn snippet(&self, column: &str, fragment_size: usize, text: &str) -> Result<String> {
        self.generators
            .get(&(column.to_ascii_lowercase(), fragment_size))
            .map(|generator| generator.snippet(text).to_html())
            .ok_or_else(|| anyhow!("missing highlight generator for {column}"))
    }
}
