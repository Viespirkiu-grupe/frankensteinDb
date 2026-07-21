use super::*;

pub(crate) fn validate_row_value(column: &ColumnDef, value: &RowValue) -> Result<()> {
    if matches!(value, RowValue::Null) {
        ensure!(column.nullable, "column {} cannot be NULL", column.name);
        return Ok(());
    }
    let valid = matches!(
        (&column.data_type, value),
        (ColumnType::Integer, RowValue::Integer(_))
            | (ColumnType::Unsigned, RowValue::Unsigned(_))
            | (ColumnType::Real, RowValue::Integer(_) | RowValue::Real(_))
            | (ColumnType::Text, RowValue::Text(_))
            | (ColumnType::Boolean, RowValue::Integer(0 | 1))
            | (ColumnType::Date, RowValue::Text(_))
            | (ColumnType::DateTime, RowValue::Text(_))
            | (ColumnType::Timestamp, RowValue::Text(_))
            | (ColumnType::TextArray, RowValue::TextArray(_))
            | (ColumnType::IntegerArray, RowValue::IntegerArray(_))
            | (ColumnType::UnsignedArray, RowValue::UnsignedArray(_))
            | (ColumnType::RealArray, RowValue::RealArray(_))
            | (ColumnType::BooleanArray, RowValue::BooleanArray(_))
            | (
                ColumnType::DateArray
                    | ColumnType::DateTimeArray
                    | ColumnType::TimestampArray
                    | ColumnType::IpArray
                    | ColumnType::FacetArray,
                RowValue::TextArray(_)
            )
            | (ColumnType::BlobArray, RowValue::BlobArray(_))
            | (ColumnType::JsonArray, RowValue::JsonArray(_))
            | (ColumnType::Blob, RowValue::Blob(_))
            | (ColumnType::Ip | ColumnType::Facet, RowValue::Text(_))
            | (ColumnType::Json, RowValue::Json(_))
            | (ColumnType::GeoPoint, RowValue::GeoPoint(_))
            | (ColumnType::GeoPointArray, RowValue::GeoPointArray(_))
    );
    ensure!(
        valid,
        "literal does not match type of column {}",
        column.name
    );
    if column.data_type == ColumnType::Date {
        let RowValue::Text(value) = value else {
            unreachable!()
        };
        chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .with_context(|| format!("invalid DATE for column {}", column.name))?;
    } else if column.data_type == ColumnType::DateTime {
        let RowValue::Text(value) = value else {
            unreachable!()
        };
        ChronoDateTime::parse_from_rfc3339(value)
            .with_context(|| format!("invalid RFC3339 DATETIME for column {}", column.name))?;
    } else if column.data_type == ColumnType::Timestamp {
        let RowValue::Text(value) = value else {
            unreachable!()
        };
        parse_timestamp(value).with_context(|| {
            format!("invalid timezone-less TIMESTAMP for column {}", column.name)
        })?;
    } else if column.data_type == ColumnType::Ip {
        let RowValue::Text(value) = value else {
            unreachable!()
        };
        value
            .parse::<std::net::IpAddr>()
            .with_context(|| format!("invalid IP address for column {}", column.name))?;
    } else if column.data_type == ColumnType::Facet {
        let RowValue::Text(value) = value else {
            unreachable!()
        };
        ensure!(
            value.starts_with('/'),
            "FACET path must start with '/': {}",
            column.name
        );
    } else if matches!(
        column.data_type,
        ColumnType::DateArray | ColumnType::DateTimeArray | ColumnType::TimestampArray
    ) {
        let RowValue::TextArray(values) = value else {
            unreachable!()
        };
        for item in values {
            match column.data_type {
                ColumnType::DateArray => {
                    chrono::NaiveDate::parse_from_str(item, "%Y-%m-%d")?;
                }
                ColumnType::DateTimeArray => {
                    ChronoDateTime::parse_from_rfc3339(item)?;
                }
                ColumnType::TimestampArray => {
                    parse_timestamp(item)?;
                }
                _ => unreachable!(),
            }
        }
    } else if column.data_type == ColumnType::IpArray {
        let RowValue::TextArray(values) = value else {
            unreachable!()
        };
        for item in values {
            item.parse::<std::net::IpAddr>()?;
        }
    } else if column.data_type == ColumnType::FacetArray {
        let RowValue::TextArray(values) = value else {
            unreachable!()
        };
        ensure!(
            values.iter().all(|value| value.starts_with('/')),
            "FACET[] paths must start with '/': {}",
            column.name
        );
    } else if matches!(
        column.data_type,
        ColumnType::GeoPoint | ColumnType::GeoPointArray
    ) {
        let points: &[crate::GeoPoint] = match value {
            RowValue::GeoPoint(point) => std::slice::from_ref(point),
            RowValue::GeoPointArray(points) => points,
            _ => unreachable!(),
        };
        ensure!(
            points.len() <= 10_000,
            "geo arrays support at most 10000 points"
        );
        for point in points {
            point.validate()?;
        }
    }
    Ok(())
}

pub(crate) fn parse_timestamp(value: &str) -> Result<NaiveDateTime> {
    Ok(NaiveDateTime::parse_from_str(
        value,
        "%Y-%m-%dT%H:%M:%S%.f",
    )?)
}

pub(crate) fn timestamp_to_tantivy(value: &str) -> Result<DateTime> {
    Ok(DateTime::from_timestamp_micros(
        parse_timestamp(value)?.and_utc().timestamp_micros(),
    ))
}

pub(crate) fn primary_key_index(def: &TableDef) -> usize {
    def.columns
        .iter()
        .position(|column| column.primary_key)
        .expect("validated schema")
}

pub(crate) fn fetch_row(
    tx: &Transaction<'_>,
    def: &TableDef,
    key: &RowValue,
) -> Result<Vec<RowValue>> {
    fetch_optional_row(tx, def, key)?.ok_or_else(|| anyhow!("row disappeared from {}", def.name))
}

pub(crate) fn fetch_optional_row(
    conn: &Connection,
    def: &TableDef,
    key: &RowValue,
) -> Result<Option<Vec<RowValue>>> {
    let primary_key = &def.columns[primary_key_index(def)];
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = ?1",
        def.columns
            .iter()
            .map(|column| quote_ident(&column.name))
            .collect::<Vec<_>>()
            .join(", "),
        quote_ident(&def.name),
        quote_ident(&primary_key.name),
    );
    let key = sqlite_value(key);
    Ok(conn
        .query_row(&sql, [key], |row| {
            (0..def.columns.len())
                .map(|idx| {
                    row_value_from_ref(&def.columns[idx], row.get_ref(idx)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            idx,
                            rusqlite::types::Type::Text,
                            error.into(),
                        )
                    })
                })
                .collect()
        })
        .optional()?)
}

pub(crate) fn row_value_from_ref(column: &ColumnDef, value: ValueRef<'_>) -> Result<RowValue> {
    let value = match value {
        ValueRef::Null => RowValue::Null,
        ValueRef::Integer(value) => RowValue::Integer(value),
        ValueRef::Real(value) => RowValue::Real(value),
        ValueRef::Text(value) => {
            let text = std::str::from_utf8(value)?;
            match column.data_type {
                ColumnType::TextArray => RowValue::TextArray(serde_json::from_str(text)?),
                ColumnType::IntegerArray => RowValue::IntegerArray(serde_json::from_str(text)?),
                ColumnType::UnsignedArray => RowValue::UnsignedArray(serde_json::from_str(text)?),
                ColumnType::RealArray => RowValue::RealArray(serde_json::from_str(text)?),
                ColumnType::BooleanArray => RowValue::BooleanArray(serde_json::from_str(text)?),
                ColumnType::DateArray
                | ColumnType::DateTimeArray
                | ColumnType::TimestampArray
                | ColumnType::IpArray
                | ColumnType::FacetArray => RowValue::TextArray(serde_json::from_str(text)?),
                ColumnType::BlobArray => RowValue::BlobArray(
                    serde_json::from_str::<Vec<String>>(text)?
                        .into_iter()
                        .map(|value| hex::decode(value.strip_prefix("0x").unwrap_or(&value)))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
                ColumnType::JsonArray => RowValue::JsonArray(serde_json::from_str(text)?),
                ColumnType::GeoPoint => RowValue::GeoPoint(serde_json::from_str(text)?),
                ColumnType::GeoPointArray => RowValue::GeoPointArray(serde_json::from_str(text)?),
                ColumnType::Unsigned => RowValue::Unsigned(text.parse()?),
                ColumnType::Json => RowValue::Json(serde_json::from_str(text)?),
                _ => RowValue::Text(text.to_owned()),
            }
        }
        ValueRef::Blob(value) => RowValue::Blob(value.to_vec()),
    };
    validate_row_value(column, &value)?;
    Ok(value)
}

pub(crate) fn sqlite_value(value: &RowValue) -> rusqlite::types::Value {
    match value {
        RowValue::Null => rusqlite::types::Value::Null,
        RowValue::Integer(value) => rusqlite::types::Value::Integer(*value),
        RowValue::Unsigned(value) => rusqlite::types::Value::Text(value.to_string()),
        RowValue::Real(value) => rusqlite::types::Value::Real(*value),
        RowValue::Text(value) => rusqlite::types::Value::Text(value.clone()),
        RowValue::TextArray(value) => {
            rusqlite::types::Value::Text(serde_json::to_string(value).expect("serializable array"))
        }
        RowValue::IntegerArray(value) => {
            rusqlite::types::Value::Text(serde_json::to_string(value).expect("serializable array"))
        }
        RowValue::UnsignedArray(value) => json_sqlite_value(value),
        RowValue::RealArray(value) => json_sqlite_value(value),
        RowValue::BooleanArray(value) => json_sqlite_value(value),
        RowValue::BlobArray(value) => rusqlite::types::Value::Text(
            serde_json::to_string(
                &value
                    .iter()
                    .map(|value| format!("0x{}", hex::encode(value)))
                    .collect::<Vec<_>>(),
            )
            .expect("serializable BLOB array"),
        ),
        RowValue::JsonArray(value) => json_sqlite_value(value),
        RowValue::Blob(value) => rusqlite::types::Value::Blob(value.clone()),
        RowValue::Json(value) => {
            rusqlite::types::Value::Text(serde_json::to_string(value).expect("serializable JSON"))
        }
        RowValue::GeoPoint(point) => rusqlite::types::Value::Text(
            serde_json::to_string(point).expect("serializable geo point"),
        ),
        RowValue::GeoPointArray(points) => rusqlite::types::Value::Text(
            serde_json::to_string(points).expect("serializable geo array"),
        ),
    }
}

/// Borrowed SQLite parameter used by bulk ingestion to avoid cloning scalar payloads.
pub(crate) struct SqliteRowValue<'a>(pub(crate) &'a RowValue);

impl rusqlite::ToSql for SqliteRowValue<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::ToSqlOutput;

        Ok(match self.0 {
            RowValue::Null => ToSqlOutput::Borrowed(ValueRef::Null),
            RowValue::Integer(value) => ToSqlOutput::Borrowed(ValueRef::Integer(*value)),
            RowValue::Unsigned(value) => {
                ToSqlOutput::Owned(rusqlite::types::Value::Text(value.to_string()))
            }
            RowValue::Real(value) => ToSqlOutput::Borrowed(ValueRef::Real(*value)),
            RowValue::Text(value) => ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes())),
            RowValue::TextArray(value) => ToSqlOutput::Owned(rusqlite::types::Value::Text(
                serde_json::to_string(value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            )),
            RowValue::IntegerArray(value) => ToSqlOutput::Owned(rusqlite::types::Value::Text(
                serde_json::to_string(value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            )),
            RowValue::UnsignedArray(value) => ToSqlOutput::Owned(json_sqlite_value(value)),
            RowValue::RealArray(value) => ToSqlOutput::Owned(json_sqlite_value(value)),
            RowValue::BooleanArray(value) => ToSqlOutput::Owned(json_sqlite_value(value)),
            RowValue::BlobArray(value) => ToSqlOutput::Owned(rusqlite::types::Value::Text(
                serde_json::to_string(
                    &value
                        .iter()
                        .map(|value| format!("0x{}", hex::encode(value)))
                        .collect::<Vec<_>>(),
                )
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            )),
            RowValue::JsonArray(value) => ToSqlOutput::Owned(json_sqlite_value(value)),
            RowValue::Blob(value) => ToSqlOutput::Borrowed(ValueRef::Blob(value)),
            RowValue::Json(value) => ToSqlOutput::Owned(rusqlite::types::Value::Text(
                serde_json::to_string(value)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            )),
            RowValue::GeoPoint(point) => ToSqlOutput::Owned(rusqlite::types::Value::Text(
                serde_json::to_string(point)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            )),
            RowValue::GeoPointArray(points) => ToSqlOutput::Owned(rusqlite::types::Value::Text(
                serde_json::to_string(points)
                    .map_err(|error| rusqlite::Error::ToSqlConversionFailure(error.into()))?,
            )),
        })
    }
}

fn json_sqlite_value(value: &impl serde::Serialize) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(serde_json::to_string(value).expect("serializable array"))
}

pub(crate) fn bulk_insert_rows(
    tx: &Transaction<'_>,
    def: &TableDef,
    rows: &[Vec<RowValue>],
) -> Result<()> {
    const SQLITE_MAX_VARIABLES: usize = 32_766;
    let column_count = def.columns.len();
    ensure!(
        column_count <= SQLITE_MAX_VARIABLES,
        "table {} has too many columns for bulk insertion",
        def.name
    );
    let rows_per_statement = SQLITE_MAX_VARIABLES / column_count;
    let columns = def
        .columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>()
        .join(", ");
    let row_placeholders = format!("({})", vec!["?"; column_count].join(", "));
    for chunk in rows.chunks(rows_per_statement) {
        let sql = format!(
            "INSERT INTO {} ({columns}) VALUES {}",
            quote_ident(&def.name),
            vec![row_placeholders.as_str(); chunk.len()].join(", ")
        );
        let mut insert = tx.prepare(&sql)?;
        insert.execute(rusqlite::params_from_iter(
            chunk.iter().flat_map(|row| row.iter().map(SqliteRowValue)),
        ))?;
    }
    Ok(())
}

pub(crate) fn bulk_upsert_rows(
    tx: &Transaction<'_>,
    def: &TableDef,
    rows: &[Vec<RowValue>],
) -> Result<()> {
    const SQLITE_MAX_VARIABLES: usize = 32_766;
    let column_count = def.columns.len();
    let rows_per_statement = SQLITE_MAX_VARIABLES / column_count;
    let columns = def
        .columns
        .iter()
        .map(|column| quote_ident(&column.name))
        .collect::<Vec<_>>();
    let primary_key = &def.columns[primary_key_index(def)].name;
    let updates = def
        .columns
        .iter()
        .filter(|column| !column.primary_key)
        .map(|column| {
            format!(
                "{} = excluded.{}",
                quote_ident(&column.name),
                quote_ident(&column.name)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let row_placeholders = format!("({})", vec!["?"; column_count].join(", "));
    for chunk in rows.chunks(rows_per_statement) {
        let sql = format!(
            "INSERT INTO {} ({}) VALUES {} ON CONFLICT ({}) DO UPDATE SET {updates}",
            quote_ident(&def.name),
            columns.join(", "),
            vec![row_placeholders.as_str(); chunk.len()].join(", "),
            quote_ident(primary_key),
        );
        tx.prepare(&sql)?.execute(rusqlite::params_from_iter(
            chunk.iter().flat_map(|row| row.iter().map(SqliteRowValue)),
        ))?;
    }
    Ok(())
}

pub(crate) fn primary_key_term(
    def: &TableDef,
    fields: &IndexFields,
    key: &RowValue,
) -> Result<Term> {
    let column = &def.columns[primary_key_index(def)];
    Ok(match (&column.data_type, key) {
        (ColumnType::Integer, RowValue::Integer(value)) => {
            Term::from_field_i64(fields.values[&column.name], *value)
        }
        (ColumnType::Text, RowValue::Text(value)) => Term::from_field_text(
            *fields.raw.get(&column.name).expect("text raw field"),
            value,
        ),
        _ => bail!("invalid primary-key value in outbox for {}", def.name),
    })
}

pub(crate) fn document_from_row(
    def: &TableDef,
    fields: &IndexFields,
    row: &[RowValue],
) -> Result<TantivyDocument> {
    ensure!(
        row.len() == def.columns.len(),
        "outbox row does not match schema for {}",
        def.name
    );
    let mut doc = TantivyDocument::new();
    for (column, value) in def.columns.iter().zip(row) {
        add_row_value(&mut doc, fields, column, value)?;
    }
    Ok(doc)
}
