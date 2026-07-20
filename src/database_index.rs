use super::*;
use crate::document_store::index_settings;

impl Database {
    pub(super) fn index_path(&self, table: &str) -> PathBuf {
        self.root.join("indexes").join(table)
    }

    pub(super) fn index_handle(&mut self, def: &TableDef) -> Result<&IndexHandle> {
        if !self.indexes.contains_key(&def.name) {
            let index = Index::open_in_dir(self.index_path(&def.name))?;
            register_analyzers(&index, def)?;
            let reader = index
                .reader_builder()
                .reload_policy(ReloadPolicy::Manual)
                .try_into()?;
            self.indexes.insert(
                def.name.clone(),
                IndexHandle {
                    index,
                    reader,
                    writer: None,
                },
            );
        }
        Ok(&self.indexes[&def.name])
    }

    pub(super) fn index_handle_mut(&mut self, def: &TableDef) -> Result<&mut IndexHandle> {
        self.index_handle(def)?;
        let handle = self
            .indexes
            .get_mut(&def.name)
            .expect("inserted index handle");
        if handle.writer.is_none() {
            handle.writer = Some(new_index_writer(&handle.index, &self.options)?);
        }
        Ok(handle)
    }

    pub(super) fn replay_outbox(&mut self) -> Result<()> {
        let ids = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM __aq_outbox ORDER BY id")?;
            stmt.query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|id| !self.deferred_outbox_ids.contains(id))
                .collect::<Vec<_>>()
        };
        self.apply_outboxes(&ids)
            .context("failed to replay indexing operations")
    }

    /// Queues operations in Tantivy's multithreaded writer without making them searchable.
    /// The durable outbox remains authoritative until a later commit succeeds.
    pub(super) fn stage_operations(
        &mut self,
        def: &TableDef,
        operations: &[RowOperation],
    ) -> Result<()> {
        let fields = {
            let handle = self.index_handle_mut(def)?;
            schema_fields(&handle.index.schema(), def)?
        };
        let handle = self
            .indexes
            .get_mut(&def.name)
            .expect("initialized index handle");
        let writer = handle.writer.as_mut().expect("initialized index writer");
        stage_row_operations(writer, def, &fields, operations)
    }

    pub(super) fn stage_outbox(&mut self, def: &TableDef, id: i64) -> Result<()> {
        let operations_json: String = self.conn.query_row(
            "SELECT operations_json FROM __aq_outbox WHERE id = ?1 AND table_name = ?2",
            params![id, def.name],
            |row| row.get(0),
        )?;
        let operations: Vec<RowOperation> = serde_json::from_str(&operations_json)?;
        let operations = self.resolve_refresh_operations(def, operations)?;
        self.stage_operations(def, &operations)
    }

    fn resolve_refresh_operations(
        &self,
        def: &TableDef,
        operations: Vec<RowOperation>,
    ) -> Result<Vec<RowOperation>> {
        operations
            .into_iter()
            .map(|operation| match operation {
                RowOperation::Refresh { key } => {
                    Ok(match fetch_optional_row(&self.conn, def, &key)? {
                        Some(row) => RowOperation::Upsert { row },
                        None => RowOperation::Delete { key },
                    })
                }
                operation => Ok(operation),
            })
            .collect()
    }

    pub(super) fn apply_outboxes(&mut self, ids: &[i64]) -> Result<()> {
        let mut grouped = BTreeMap::<String, Vec<i64>>::new();
        for id in ids {
            let table_name: Option<String> = self
                .conn
                .query_row(
                    "SELECT table_name FROM __aq_outbox WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(table_name) = table_name {
                grouped.entry(table_name).or_default().push(*id);
            }
        }

        for (table_name, record_ids) in grouped {
            let def = self.table(&table_name)?;
            for id in &record_ids {
                if self.staged_outbox_ids.contains(id) {
                    continue;
                }
                self.stage_outbox(&def, *id)?;
            }
            {
                self.index_handle_mut(&def)?;
                let handle = self
                    .indexes
                    .get_mut(&def.name)
                    .expect("initialized index handle");
                let writer = handle.writer.as_mut().expect("initialized index writer");
                let first_id = record_ids[0];
                let last_id = *record_ids.last().expect("non-empty outbox group");
                let payload = if first_id == last_id {
                    format!("outbox={first_id}")
                } else {
                    format!("outbox={first_id}..{last_id}")
                };
                let mut prepared = writer.prepare_commit()?;
                prepared.set_payload(&payload);
                prepared.commit()?;
                handle.reader.reload()?;
            }
            let tx = self.conn.transaction()?;
            for id in &record_ids {
                tx.execute("DELETE FROM __aq_outbox WHERE id = ?1", [id])?;
            }
            tx.commit()?;
            for id in record_ids {
                self.deferred_outbox_ids.remove(&id);
                self.staged_outbox_ids.remove(&id);
            }
        }
        Ok(())
    }

    pub(super) fn rebuild_table(&mut self, table: &str) -> Result<usize> {
        let def = self.table(table)?;
        self.indexes.remove(&def.name);
        let final_path = self.index_path(table);
        let build_path = self.root.join("indexes").join(format!(".{table}.building"));
        if build_path.exists() {
            fs::remove_dir_all(&build_path)?;
        }
        fs::create_dir_all(&build_path)?;
        let schema = build_tantivy_schema(&def);
        let index = Index::builder()
            .schema(schema.clone())
            .settings(index_settings(&def))
            .create_in_dir(&build_path)?;
        register_analyzers(&index, &def)?;
        let mut writer = new_index_writer(&index, &self.options)?;
        let fields = schema_fields(&schema, &def)?;
        let select_sql = format!(
            "SELECT {} FROM {}",
            def.columns
                .iter()
                .map(|c| quote_ident(&c.name))
                .collect::<Vec<_>>()
                .join(", "),
            quote_ident(table)
        );
        let mut stmt = self.conn.prepare(&select_sql)?;
        let mut rows = stmt.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            let mut doc = TantivyDocument::new();
            for (idx, column) in def.columns.iter().enumerate() {
                add_sql_value(&mut doc, &fields, column, row.get_ref(idx)?)?;
            }
            writer.add_document(doc)?;
            count += 1;
        }
        let mut prepared = writer.prepare_commit()?;
        prepared.set_payload(&format!("rows={count}"));
        prepared.commit()?;
        drop(writer);
        drop(index);
        swap_index_generation(&final_path, &build_path)?;
        self.conn
            .execute("UPDATE __aq_tables SET dirty = 0 WHERE name = ?1", [table])?;
        Ok(count)
    }
}

/// Installs a fully committed generation while existing mmap readers keep the retired files open.
fn swap_index_generation(final_path: &Path, build_path: &Path) -> Result<()> {
    let retired_path = final_path.with_extension("retired");
    if retired_path.exists() {
        fs::remove_dir_all(&retired_path)?;
    }
    let had_previous = final_path.exists();
    if had_previous {
        fs::rename(final_path, &retired_path)?;
    }
    if let Err(error) = fs::rename(build_path, final_path) {
        if had_previous {
            let _ = fs::rename(&retired_path, final_path);
        }
        return Err(error.into());
    }
    if had_previous {
        fs::remove_dir_all(retired_path)?;
    }
    Ok(())
}

fn stage_row_operations(
    writer: &mut IndexWriter,
    def: &TableDef,
    fields: &IndexFields,
    operations: &[RowOperation],
) -> Result<()> {
    let primary_key = primary_key_index(def);
    for operation in operations {
        let key = match operation {
            RowOperation::Delete { key } => key,
            RowOperation::Upsert { row } => &row[primary_key],
            RowOperation::Refresh { .. } => {
                bail!("unresolved refresh operation for {}", def.name)
            }
        };
        writer.delete_term(primary_key_term(def, fields, key)?);
        if let RowOperation::Upsert { row } = operation {
            writer.add_document(document_from_row(def, fields, row)?)?;
        }
    }
    Ok(())
}
