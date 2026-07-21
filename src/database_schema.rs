use super::*;

impl Database {
    /// Atomically replaces a table schema after strictly converting every authoritative row.
    pub fn change_table_schema(
        &mut self,
        table: &str,
        changes: Vec<SchemaChange>,
    ) -> Result<QueryResult> {
        self.flush()?;
        let old = self.table(table)?;
        let plan = SchemaPlan::build(&old, changes)?;
        validate_table_def(&plan.target)?;
        ensure!(
            old.columns[primary_key_index(&old)].name
                == plan.target.columns[primary_key_index(&plan.target)].name
                && old.columns[primary_key_index(&old)].data_type
                    == plan.target.columns[primary_key_index(&plan.target)].data_type,
            "primary-key rename or type change is not supported"
        );
        let shadow_name = format!("__aq_migrate_{}", old.name);
        let mut shadow = plan.target.clone();
        shadow.name = shadow_name.clone();
        let tx = self.conn.transaction()?;
        tx.execute_batch(&format!(
            "DROP TABLE IF EXISTS {}",
            quote_ident(&shadow_name)
        ))?;
        tx.execute_batch(&sqlite_create_sql(&shadow))?;
        copy_converted_rows(&tx, &old, &shadow, &plan)?;
        tx.execute_batch(&format!(
            "DROP TABLE {}; ALTER TABLE {} RENAME TO {}",
            quote_ident(&old.name),
            quote_ident(&shadow_name),
            quote_ident(&old.name),
        ))?;
        tx.execute(
            "UPDATE __aq_tables SET schema_json=?1, dirty=1 WHERE name=?2",
            params![serde_json::to_string(&plan.target)?, old.name],
        )?;
        tx.commit()?;
        let count = self.rebuild_table(&old.name)?;
        Ok(QueryResult::message(format!(
            "migrated {count} row(s) in {}",
            old.name
        )))
    }
}

struct SchemaPlan {
    target: TableDef,
    sources: HashMap<String, String>,
    defaults: HashMap<String, Value>,
}

impl SchemaPlan {
    fn build(old: &TableDef, changes: Vec<SchemaChange>) -> Result<Self> {
        let mut target = old.clone();
        let mut sources = old
            .columns
            .iter()
            .map(|column| (column.name.clone(), column.name.clone()))
            .collect::<HashMap<_, _>>();
        let mut defaults = HashMap::new();
        for change in changes {
            match change {
                SchemaChange::AddColumn { column, default } => {
                    ensure!(
                        target
                            .columns
                            .iter()
                            .all(|item| !item.name.eq_ignore_ascii_case(&column.name)),
                        "column already exists: {}",
                        column.name
                    );
                    if !column.nullable {
                        ensure!(
                            !default.is_null(),
                            "non-nullable added column requires a default"
                        );
                    }
                    defaults.insert(column.name.clone(), default);
                    target.columns.push(column);
                }
                SchemaChange::DropColumn { column } => {
                    let index = find_column_index(&target, &column)?;
                    ensure!(
                        !target.columns[index].primary_key,
                        "primary key cannot be dropped"
                    );
                    let removed = target.columns.remove(index);
                    sources.remove(&removed.name);
                    defaults.remove(&removed.name);
                }
                SchemaChange::RenameColumn { from, to } => {
                    let index = find_column_index(&target, &from)?;
                    ensure!(
                        !target.columns[index].primary_key,
                        "primary key cannot be renamed"
                    );
                    ensure!(
                        target
                            .columns
                            .iter()
                            .all(|item| !item.name.eq_ignore_ascii_case(&to)),
                        "column already exists: {to}"
                    );
                    target.columns[index].name = to.clone();
                    if let Some(source) = sources.remove(&from) {
                        sources.insert(to.clone(), source);
                    }
                    if let Some(default) = defaults.remove(&from) {
                        defaults.insert(to, default);
                    }
                }
                SchemaChange::AlterColumn { column, definition } => {
                    let index = find_column_index(&target, &column)?;
                    ensure!(
                        !target.columns[index].primary_key,
                        "primary key cannot be altered"
                    );
                    ensure!(
                        definition.name.eq_ignore_ascii_case(&column),
                        "alter definition must retain the column name"
                    );
                    target.columns[index] = definition;
                }
                SchemaChange::AlterDocumentStore { document_store } => {
                    target.document_store = document_store;
                }
            }
        }
        Ok(Self {
            target,
            sources,
            defaults,
        })
    }
}

fn copy_converted_rows(
    tx: &Transaction<'_>,
    old: &TableDef,
    shadow: &TableDef,
    plan: &SchemaPlan,
) -> Result<()> {
    let sql = format!(
        "SELECT {} FROM {}",
        old.columns
            .iter()
            .map(|column| quote_ident(&column.name))
            .collect::<Vec<_>>()
            .join(", "),
        quote_ident(&old.name)
    );
    let mut statement = tx.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let old_indexes = old
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| (column.name.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut batch = Vec::with_capacity(2_000);
    while let Some(row) = rows.next()? {
        let mut converted = Vec::with_capacity(shadow.columns.len());
        for target in &shadow.columns {
            let value = if let Some(source) = plan.sources.get(&target.name) {
                let source_column = &old.columns[old_indexes[source]];
                let value = row_value_from_ref(source_column, row.get_ref(old_indexes[source])?)?;
                convert_value(target, value)?
            } else {
                json_to_row_value(target, &plan.defaults[&target.name])?
            };
            converted.push(value);
        }
        batch.push(converted);
        if batch.len() == 2_000 {
            bulk_insert_rows(tx, shadow, &batch)?;
            batch.clear();
        }
    }
    bulk_insert_rows(tx, shadow, &batch)
}

fn convert_value(target: &ColumnDef, value: RowValue) -> Result<RowValue> {
    if matches!(value, RowValue::Null) {
        return json_to_row_value(target, &Value::Null);
    }
    let json = match value {
        RowValue::Null => Value::Null,
        RowValue::Integer(value) => match target.data_type {
            ColumnType::Boolean if matches!(value, 0 | 1) => json!(value == 1),
            ColumnType::Text => json!(value.to_string()),
            _ => json!(value),
        },
        RowValue::Unsigned(value) => match target.data_type {
            ColumnType::Text => json!(value.to_string()),
            _ => json!(value),
        },
        RowValue::Real(value) => match target.data_type {
            ColumnType::Integer if value.fract() == 0.0 => json!(value as i64),
            ColumnType::Text => json!(value.to_string()),
            _ => json!(value),
        },
        RowValue::Text(value) => match target.data_type {
            ColumnType::Integer => json!(value.parse::<i64>()?),
            ColumnType::Real => json!(value.parse::<f64>()?),
            ColumnType::Boolean => json!(value.parse::<bool>()?),
            _ => json!(value),
        },
        RowValue::TextArray(value) => json!(value),
        RowValue::IntegerArray(value) => json!(value),
        RowValue::UnsignedArray(value) => json!(value),
        RowValue::RealArray(value) => json!(value),
        RowValue::BooleanArray(value) => json!(value),
        RowValue::BlobArray(value) => json!(
            value
                .into_iter()
                .map(|value| format!("0x{}", hex::encode(value)))
                .collect::<Vec<_>>()
        ),
        RowValue::JsonArray(value) => json!(value),
        RowValue::Blob(value) => json!(format!("0x{}", hex::encode(value))),
        RowValue::Json(value) => value,
        RowValue::GeoPoint(value) => json!(value),
        RowValue::GeoPointArray(value) => json!(value),
    };
    json_to_row_value(target, &json)
}

fn find_column_index(def: &TableDef, name: &str) -> Result<usize> {
    def.columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| anyhow!("unknown column: {name}"))
}
