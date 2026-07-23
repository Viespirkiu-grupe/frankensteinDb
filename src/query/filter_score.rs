use super::*;

pub(crate) fn filter_contributes_score(filter: &Filter) -> bool {
    match filter {
        Filter::Search { .. }
        | Filter::SearchBoosted { .. }
        | Filter::Fuzzy { .. }
        | Filter::Prefix { .. }
        | Filter::PhrasePrefix { .. }
        | Filter::DisjunctionMax { .. }
        | Filter::Regex { .. }
        | Filter::RegexPhrase { .. }
        | Filter::JsonSearch { .. } => true,
        Filter::All { filters } | Filter::Any { filters } => {
            filters.iter().any(filter_contributes_score)
        }
        Filter::Not { .. }
        | Filter::Compare { .. }
        | Filter::Between { .. }
        | Filter::In { .. }
        | Filter::IsNull { .. }
        | Filter::JsonCompare { .. }
        | Filter::JsonBetween { .. }
        | Filter::JsonExists { .. }
        | Filter::GeoDistance { .. }
        | Filter::GeoBoundingBox { .. }
        | Filter::GeoDistanceCompare { .. } => false,
    }
}
