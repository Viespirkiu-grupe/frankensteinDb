use anyhow::{Result, bail, ensure};
use tantivy::tokenizer::Language;

use super::quote_ident;
use crate::document_store::validate_document_store;
use crate::{Analyzer, ColumnType, TableDef};

pub(super) fn validate_table_def(def: &TableDef) -> Result<()> {
    validate_document_store(&def.document_store)?;
    ensure!(
        !def.name.starts_with("__aq_"),
        "table names beginning with __aq_ are reserved"
    );
    ensure!(!def.columns.is_empty(), "a table must contain columns");
    let mut aliases = std::collections::HashSet::new();
    for alias in &def.aliases {
        ensure!(!alias.trim().is_empty(), "table alias cannot be empty");
        ensure!(!alias.starts_with("__aq_"), "reserved table alias: {alias}");
        ensure!(
            !alias.eq_ignore_ascii_case(&def.name),
            "alias duplicates table name: {alias}"
        );
        ensure!(
            aliases.insert(alias.to_ascii_lowercase()),
            "duplicate table alias: {alias}"
        );
    }
    let primary_keys = def
        .columns
        .iter()
        .filter(|column| column.primary_key)
        .collect::<Vec<_>>();
    ensure!(
        primary_keys.len() == 1,
        "exactly one primary key column is required"
    );
    ensure!(
        matches!(
            primary_keys[0].data_type,
            ColumnType::Integer | ColumnType::Text
        ),
        "the primary key must be INTEGER or TEXT"
    );
    let mut seen = std::collections::HashSet::new();
    for column in &def.columns {
        ensure!(
            !column.name.starts_with("__aq_"),
            "column names beginning with __aq_ are reserved"
        );
        ensure!(
            !column.name.eq_ignore_ascii_case("_score"),
            "_score is a reserved virtual column"
        );
        ensure!(
            seen.insert(column.name.to_ascii_lowercase()),
            "duplicate column: {}",
            column.name
        );
        let searchable = matches!(column.data_type, ColumnType::Text | ColumnType::TextArray);
        ensure!(
            searchable == column.analyzer.is_some(),
            "only TEXT and TEXT[] columns require an analyzer: {}",
            column.name
        );
        if let Some(analyzer) = &column.analyzer {
            validate_analyzer(analyzer)?;
        }
        if matches!(
            column.data_type,
            ColumnType::Facet
                | ColumnType::FacetArray
                | ColumnType::GeoPoint
                | ColumnType::GeoPointArray
        ) {
            ensure!(
                column.index.indexed,
                "FACET and GEO_POINT columns are always indexed: {}",
                column.name
            );
        }
    }
    Ok(())
}

fn validate_analyzer(analyzer: &Analyzer) -> Result<()> {
    match analyzer {
        Analyzer::Stem(name) => {
            language(name)?;
        }
        Analyzer::Ngram { min, max, .. } => ensure!(
            *min > 0 && min <= max && *max <= 32,
            "ngram bounds must satisfy 1 <= min <= max <= 32"
        ),
        Analyzer::Custom {
            stem,
            stop_words,
            synonyms,
            ..
        } => {
            if let Some(name) = stem {
                language(name)?;
            }
            ensure!(
                stop_words.len() <= 10_000,
                "at most 10000 stop words are allowed"
            );
            ensure!(
                stop_words.iter().all(|word| !word.is_empty()),
                "stop words cannot be empty"
            );
            ensure!(
                synonyms.len() <= 10_000,
                "at most 10000 synonym keys are allowed"
            );
            ensure!(
                synonyms.iter().all(|(term, expansions)| {
                    !term.is_empty()
                        && term == &term.to_lowercase()
                        && !expansions.is_empty()
                        && expansions.iter().all(|value| !value.is_empty())
                }),
                "synonyms require lowercase non-empty keys and expansions"
            );
        }
        Analyzer::Default | Analyzer::Raw | Analyzer::Whitespace => {}
    }
    Ok(())
}

pub(super) fn language(name: &str) -> Result<Language> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "arabic" => Language::Arabic,
        "danish" => Language::Danish,
        "dutch" => Language::Dutch,
        "english" => Language::English,
        "finnish" => Language::Finnish,
        "french" => Language::French,
        "german" => Language::German,
        "greek" => Language::Greek,
        "hungarian" => Language::Hungarian,
        "italian" => Language::Italian,
        "norwegian" => Language::Norwegian,
        "portuguese" => Language::Portuguese,
        "romanian" => Language::Romanian,
        "russian" => Language::Russian,
        "spanish" => Language::Spanish,
        "swedish" => Language::Swedish,
        "tamil" => Language::Tamil,
        "turkish" => Language::Turkish,
        _ => bail!("unsupported stemming language: {name}"),
    })
}

pub(super) fn analyzer_name(column: &crate::ColumnDef) -> String {
    match column.analyzer.as_ref().expect("text analyzer") {
        Analyzer::Default => "default".into(),
        Analyzer::Raw => "raw".into(),
        Analyzer::Whitespace => "whitespace".into(),
        Analyzer::Stem(language) => format!("aq_stem_{language}"),
        Analyzer::Ngram {
            min,
            max,
            prefix_only,
        } => format!("aq_ngram_{min}_{max}_{prefix_only}"),
        Analyzer::Custom { .. } => format!("aq_custom_{}", column.name),
    }
}

/// Generates the private SQLite table definition used by the storage adapter.
pub(super) fn sqlite_create_sql(def: &TableDef) -> String {
    let columns = def
        .columns
        .iter()
        .map(|column| {
            let data_type = match column.data_type {
                ColumnType::Integer | ColumnType::Boolean => "INTEGER",
                ColumnType::Real => "REAL",
                ColumnType::Text
                | ColumnType::Date
                | ColumnType::DateTime
                | ColumnType::Timestamp
                | ColumnType::TextArray
                | ColumnType::IntegerArray
                | ColumnType::UnsignedArray
                | ColumnType::RealArray
                | ColumnType::BooleanArray
                | ColumnType::DateArray
                | ColumnType::DateTimeArray
                | ColumnType::TimestampArray
                | ColumnType::BlobArray
                | ColumnType::IpArray
                | ColumnType::JsonArray
                | ColumnType::FacetArray
                | ColumnType::GeoPoint
                | ColumnType::GeoPointArray => "TEXT",
                ColumnType::Unsigned
                | ColumnType::Ip
                | ColumnType::Json
                | ColumnType::Facet => "TEXT",
                ColumnType::Blob => "BLOB",
            };
            let mut constraints = Vec::new();
            if column.primary_key {
                constraints.push("PRIMARY KEY");
            }
            if !column.nullable && !column.primary_key {
                constraints.push("NOT NULL");
            }
            if column.data_type == ColumnType::Boolean {
                constraints.push(if column.nullable {
                    "CHECK (\"__COLUMN__\" IS NULL OR (typeof(\"__COLUMN__\") = 'integer' AND \"__COLUMN__\" IN (0, 1)))"
                } else {
                    "CHECK (typeof(\"__COLUMN__\") = 'integer' AND \"__COLUMN__\" IN (0, 1))"
                });
            }
            let constraints = constraints
                .join(" ")
                .replace("__COLUMN__", &column.name.replace('"', "\"\""));
            format!(
                "{} {} {}",
                quote_ident(&column.name),
                data_type,
                constraints
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("CREATE TABLE {} ({columns}) STRICT", quote_ident(&def.name))
}
