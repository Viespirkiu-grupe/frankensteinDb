use super::*;
use sha2::{Digest, Sha256};

impl Database {
    /// Verifies a strong row ETag against authoritative SQLite inside the serialized writer
    /// boundary. Public row reads still obtain the same logical values exclusively from Tantivy.
    pub fn ensure_row_etag(&self, table: &str, key: &str, expected: &str) -> Result<()> {
        let def = self.table(table)?;
        let primary_key = &def.columns[primary_key_index(&def)];
        let key = json_to_row_value(
            primary_key,
            &match primary_key.data_type {
                ColumnType::Integer => json!(key.parse::<i64>()?),
                ColumnType::Text => json!(key),
                _ => bail!("unsupported primary key type"),
            },
        )?;
        let row = fetch_optional_row(&self.conn, &def, &key)?
            .ok_or_else(|| anyhow!("row not found: {key:?}"))?;
        let object = def
            .columns
            .iter()
            .zip(row)
            .map(|(column, value)| Ok((column.name.clone(), etag_value(column, value)?)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        let actual = format!(
            "\"{}\"",
            hex::encode(Sha256::digest(serde_json::to_vec(&object)?))
        );
        ensure!(actual == expected, "etag mismatch");
        Ok(())
    }
    /// Applies a filtered update/delete only when the published Tantivy match set fits `max_rows`.
    /// Matching keys are resolved once, so the limit check and mutation use identical membership.
    pub fn mutate_typed_limited(
        &mut self,
        mutation: Mutation,
        max_rows: Option<usize>,
    ) -> Result<QueryResult> {
        self.mutate_typed_with_limit(mutation, true, max_rows)
    }

    /// Atomically applies typed mutations and publishes their Tantivy changes together.
    /// Every update/delete filter is resolved against the Tantivy snapshot visible before the
    /// batch; mutations earlier in the same batch do not change later filter membership.
    pub fn mutate_batch_typed(&mut self, mutations: Vec<Mutation>) -> Result<Vec<QueryResult>> {
        self.flush()?;
        let mut planned = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            planned.push(self.plan_typed_mutation(mutation)?);
        }
        let tx = self.conn.transaction()?;
        let mut results = Vec::with_capacity(planned.len());
        let mut outbox_ids = Vec::new();
        for mutation in planned {
            let (def, changed, operations) = apply_planned_mutation(&tx, mutation)?;
            if let Some(id) = insert_typed_outbox(&tx, &def, &operations)? {
                outbox_ids.push(id);
            }
            results.push(QueryResult::message(format!("{changed} row(s) changed")));
        }
        tx.commit()?;
        self.apply_outboxes(&outbox_ids)?;
        Ok(results)
    }

    /// Applies one typed mutation and publishes it to Tantivy before returning.
    pub fn mutate_typed(&mut self, mutation: Mutation) -> Result<QueryResult> {
        self.mutate_typed_with_limit(mutation, true, None)
    }

    /// Durably applies one typed mutation while deferring Tantivy visibility until `flush`.
    pub fn mutate_typed_deferred(&mut self, mutation: Mutation) -> Result<QueryResult> {
        self.mutate_typed_with_limit(mutation, false, None)
    }

    fn mutate_typed_with_limit(
        &mut self,
        mutation: Mutation,
        publish: bool,
        max_rows: Option<usize>,
    ) -> Result<QueryResult> {
        self.recover()?;
        if let Mutation::Insert { table, row } = mutation {
            return self.insert_typed_row(&table, row, publish);
        }
        let (table, filter) = mutation_target(&mutation);
        let def = self.table(table)?;
        let keys = self.matching_primary_keys(&def, filter.clone())?;
        if let Some(max_rows) = max_rows {
            ensure!(
                keys.len() <= max_rows,
                "mutation matched {} rows, exceeding max_rows {max_rows}",
                keys.len()
            );
        }
        let (changed, outbox_id) = match mutation {
            Mutation::Update { values, .. } => {
                let assignments = typed_assignments(&def, values)?;
                self.persist_typed_update(&def, &assignments, &keys)?
            }
            Mutation::Delete { .. } => self.persist_typed_delete(&def, &keys)?,
            Mutation::Insert { .. } => unreachable!(),
        };
        self.publish_typed_outbox(&def, outbox_id, publish)?;
        Ok(QueryResult::message(format!("{changed} row(s) changed")))
    }

    fn insert_typed_row(
        &mut self,
        table: &str,
        row: BTreeMap<String, Value>,
        publish: bool,
    ) -> Result<QueryResult> {
        let def = self.table(table)?;
        ensure_known_fields(&def, row.keys())?;
        let values = def
            .columns
            .iter()
            .map(|column| row.get(&column.name).cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>();
        let changed = self.bulk_insert_json_deferred(table, &[values])?;
        if publish {
            self.flush()?;
        }
        Ok(QueryResult::message(format!("{changed} row(s) changed")))
    }

    fn matching_primary_keys(&mut self, def: &TableDef, filter: Filter) -> Result<Vec<RowValue>> {
        let primary_key = &def.columns[primary_key_index(def)];
        let (index, reader) = {
            let handle = self.index_handle(def)?;
            (handle.index.clone(), handle.reader.clone())
        };
        let fields = schema_fields(&index.schema(), def)?;
        let searcher = reader.searcher();
        validate_filter_only_json_paths(&searcher, def, Some(&filter))?;
        let query = compile_filter(&index, def, &fields, Some(&filter))?.query;
        let addresses = searcher.search(&*query, &DocSetCollector)?;
        let projected = [primary_key];
        let mut segment_readers = HashMap::new();
        addresses
            .into_iter()
            .map(|address| {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    segment_readers.entry(address.segment_ord)
                {
                    entry.insert(segment_fast_readers(
                        &searcher,
                        address.segment_ord,
                        &projected,
                    )?);
                }
                let value = segment_readers[&address.segment_ord][0].value(address.doc_id)?;
                json_to_row_value(primary_key, &value)
            })
            .collect()
    }

    fn persist_typed_update(
        &mut self,
        def: &TableDef,
        assignments: &[(String, RowValue)],
        keys: &[RowValue],
    ) -> Result<(usize, Option<i64>)> {
        let tx = self.conn.transaction()?;
        let changed = update_rows_by_key(&tx, def, assignments, keys)?;
        let operations = keys
            .iter()
            .map(|key| {
                Ok(RowOperation::Upsert {
                    row: fetch_row(&tx, def, key)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let outbox_id = insert_typed_outbox(&tx, def, &operations)?;
        tx.commit()?;
        Ok((changed, outbox_id))
    }

    fn persist_typed_delete(
        &mut self,
        def: &TableDef,
        keys: &[RowValue],
    ) -> Result<(usize, Option<i64>)> {
        let tx = self.conn.transaction()?;
        let changed = delete_rows_by_key(&tx, def, keys)?;
        let operations = keys
            .iter()
            .cloned()
            .map(|key| RowOperation::Delete { key })
            .collect::<Vec<_>>();
        let outbox_id = insert_typed_outbox(&tx, def, &operations)?;
        tx.commit()?;
        Ok((changed, outbox_id))
    }

    fn publish_typed_outbox(
        &mut self,
        def: &TableDef,
        outbox_id: Option<i64>,
        publish: bool,
    ) -> Result<()> {
        let Some(id) = outbox_id else {
            return Ok(());
        };
        if publish {
            self.apply_outboxes(&[id])
        } else {
            self.stage_outbox(def, id)?;
            self.deferred_outbox_ids.insert(id);
            self.staged_outbox_ids.insert(id);
            Ok(())
        }
    }

    fn plan_typed_mutation(&mut self, mutation: Mutation) -> Result<PlannedMutation> {
        match mutation {
            Mutation::Insert { table, row } => {
                let def = self.table(&table)?;
                ensure_known_fields(&def, row.keys())?;
                let row = def
                    .columns
                    .iter()
                    .map(|column| {
                        json_to_row_value(column, row.get(&column.name).unwrap_or(&Value::Null))
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(PlannedMutation::Insert { def, row })
            }
            Mutation::Update {
                table,
                values,
                filter,
            } => {
                let def = self.table(&table)?;
                let keys = self.matching_primary_keys(&def, filter)?;
                Ok(PlannedMutation::Update {
                    assignments: typed_assignments(&def, values)?,
                    def,
                    keys,
                })
            }
            Mutation::Delete { table, filter } => {
                let def = self.table(&table)?;
                let keys = self.matching_primary_keys(&def, filter)?;
                Ok(PlannedMutation::Delete { def, keys })
            }
        }
    }
}

fn etag_value(column: &ColumnDef, value: RowValue) -> Result<Value> {
    Ok(match value {
        RowValue::Null => Value::Null,
        RowValue::Integer(value) if column.data_type == ColumnType::Boolean => json!(value == 1),
        RowValue::Integer(value) => json!(value),
        RowValue::Unsigned(value) => json!(value),
        RowValue::Real(value) => json!(value),
        RowValue::Text(value) => json!(value),
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
    })
}

enum PlannedMutation {
    Insert {
        def: TableDef,
        row: Vec<RowValue>,
    },
    Update {
        def: TableDef,
        assignments: Vec<(String, RowValue)>,
        keys: Vec<RowValue>,
    },
    Delete {
        def: TableDef,
        keys: Vec<RowValue>,
    },
}

fn apply_planned_mutation(
    tx: &Transaction<'_>,
    mutation: PlannedMutation,
) -> Result<(TableDef, usize, Vec<RowOperation>)> {
    match mutation {
        PlannedMutation::Insert { def, row } => {
            bulk_insert_rows(tx, &def, std::slice::from_ref(&row))?;
            Ok((def, 1, vec![RowOperation::Upsert { row }]))
        }
        PlannedMutation::Update {
            def,
            assignments,
            keys,
        } => {
            let changed = update_rows_by_key(tx, &def, &assignments, &keys)?;
            let operations = keys
                .iter()
                .map(|key| {
                    Ok(RowOperation::Upsert {
                        row: fetch_row(tx, &def, key)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok((def, changed, operations))
        }
        PlannedMutation::Delete { def, keys } => {
            let changed = delete_rows_by_key(tx, &def, &keys)?;
            let operations = keys
                .into_iter()
                .map(|key| RowOperation::Delete { key })
                .collect();
            Ok((def, changed, operations))
        }
    }
}

fn mutation_target(mutation: &Mutation) -> (&str, &Filter) {
    match mutation {
        Mutation::Update { table, filter, .. } | Mutation::Delete { table, filter } => {
            (table, filter)
        }
        Mutation::Insert { .. } => unreachable!(),
    }
}

fn typed_assignments(
    def: &TableDef,
    values: BTreeMap<String, Value>,
) -> Result<Vec<(String, RowValue)>> {
    ensure!(!values.is_empty(), "update requires at least one value");
    ensure_known_fields(def, values.keys())?;
    values
        .into_iter()
        .map(|(name, value)| {
            let column = column(def, &name)?;
            ensure!(!column.primary_key, "primary keys cannot be updated");
            Ok((column.name.clone(), json_to_row_value(column, &value)?))
        })
        .collect()
}

fn ensure_known_fields<'a>(def: &TableDef, names: impl Iterator<Item = &'a String>) -> Result<()> {
    for name in names {
        column(def, name)?;
    }
    Ok(())
}

fn insert_typed_outbox(
    tx: &Transaction<'_>,
    def: &TableDef,
    operations: &[RowOperation],
) -> Result<Option<i64>> {
    if operations.is_empty() {
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO __aq_outbox(table_name, operations_json) VALUES (?1, ?2)",
        params![def.name, serde_json::to_string(operations)?],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}
