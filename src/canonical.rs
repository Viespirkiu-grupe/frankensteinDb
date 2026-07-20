use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::{Analyzer, ColumnDef, ColumnType, TableDef};

/// Flattened schema for the canonical VPM contract feed. `dokumentai` is deliberately omitted;
/// the primary and additional suppliers/CPV codes become equivalent array values.
pub fn canonical_contract_table() -> TableDef {
    TableDef {
        name: "sutartys".into(),
        aliases: vec![],
        document_store: Default::default(),
        columns: vec![
            column("unikalusId", ColumnType::Integer, true, false, None),
            column(
                "pavadinimas",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Default),
            ),
            column("sudarymoData", ColumnType::Date, false, true, None),
            column("galiojimoData", ColumnType::Date, false, true, None),
            column("faktineIvykdimoData", ColumnType::Date, false, true, None),
            column("paskelbimoData", ColumnType::Timestamp, false, true, None),
            column("redagavimoData", ColumnType::Timestamp, false, true, None),
            column(
                "perkanciosiosOrganizacijosKodas",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Raw),
            ),
            column(
                "perkanciosiosOrganizacijosPavadinimas",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Default),
            ),
            column(
                "sutartiesNumeris",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Raw),
            ),
            column(
                "pirkimoNumeris",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Raw),
            ),
            column("numatomaVerte", ColumnType::Real, false, true, None),
            column("faktineVerte", ColumnType::Real, false, true, None),
            column(
                "tiekejuKodai",
                ColumnType::TextArray,
                false,
                true,
                Some(Analyzer::Raw),
            ),
            column(
                "tiekejuPavadinimai",
                ColumnType::TextArray,
                false,
                true,
                Some(Analyzer::Default),
            ),
            column(
                "tipas",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Default),
            ),
            column(
                "kategorija",
                ColumnType::Text,
                false,
                true,
                Some(Analyzer::Default),
            ),
            column("bvpzKodai", ColumnType::IntegerArray, false, true, None),
            column("istrinta", ColumnType::Boolean, false, false, None),
            column("pakeitimas", ColumnType::Boolean, false, false, None),
        ],
    }
}

fn column(
    name: &str,
    data_type: ColumnType,
    primary_key: bool,
    nullable: bool,
    analyzer: Option<Analyzer>,
) -> ColumnDef {
    let compact_raw = analyzer == Some(Analyzer::Raw);
    ColumnDef {
        name: name.into(),
        data_type,
        primary_key,
        nullable,
        analyzer,
        compact_raw,
        index: Default::default(),
    }
}

/// Converts one canonical JSON object into the column order used by
/// [`canonical_contract_table`]. Null supplier values are omitted from arrays.
pub fn canonical_contract_row(document: &Value) -> Result<Vec<Value>> {
    let object = document
        .as_object()
        .ok_or_else(|| anyhow!("canonical contract must be a JSON object"))?;
    let required = |name: &str| {
        object
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("canonical contract is missing {name}"))
    };

    let mut supplier_codes = Vec::new();
    let mut supplier_names = Vec::new();
    if let Some(value) = object.get("pirmoTiekejoKodas").and_then(Value::as_str) {
        supplier_codes.push(json!(value));
    }
    if let Some(value) = object
        .get("pirmoTiekejoPavadinimas")
        .and_then(Value::as_str)
    {
        supplier_names.push(json!(value));
    }
    let additional = object
        .get("papildomiTiekejai")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("papildomiTiekejai must be an array"))?;
    for supplier in additional {
        let supplier = supplier
            .as_object()
            .ok_or_else(|| anyhow!("papildomiTiekejai items must be objects"))?;
        if let Some(value) = supplier.get("kodas").and_then(Value::as_str) {
            supplier_codes.push(json!(value));
        }
        if let Some(value) = supplier.get("pavadinimas").and_then(Value::as_str) {
            supplier_names.push(json!(value));
        }
    }

    let mut cpv_codes = Vec::new();
    if let Some(value) = object.get("bvpzKodas").and_then(Value::as_i64) {
        cpv_codes.push(json!(value));
    }
    let additional_cpv = object
        .get("papildomiBvpzKodai")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("papildomiBvpzKodai must be an array"))?;
    for value in additional_cpv {
        cpv_codes.push(json!(value.as_i64().ok_or_else(|| {
            anyhow!("papildomiBvpzKodai items must be integers")
        })?));
    }

    Ok(vec![
        required("unikalusId")?,
        required("pavadinimas")?,
        required("sudarymoData")?,
        required("galiojimoData")?,
        required("faktineIvykdimoData")?,
        required("paskelbimoData")?,
        required("redagavimoData")?,
        required("perkanciosiosOrganizacijosKodas")?,
        required("perkanciosiosOrganizacijosPavadinimas")?,
        required("sutartiesNumeris")?,
        required("pirkimoNumeris")?,
        required("numatomaVerte")?,
        required("faktineVerte")?,
        Value::Array(supplier_codes),
        Value::Array(supplier_names),
        required("tipas")?,
        required("kategorija")?,
        Value::Array(cpv_codes),
        required("istrinta")?,
        required("pakeitimas")?,
    ])
}
