use super::*;

impl Database {
    /// Validates one schema-ordered JSON row without writing it.
    pub fn validate_json_row(&self, table: &str, values: &[Value]) -> Result<()> {
        let def = self.table(table)?;
        ensure!(
            values.len() == def.columns.len(),
            "row has {} values for {} columns",
            values.len(),
            def.columns.len()
        );
        for (column, value) in def.columns.iter().zip(values) {
            json_to_row_value(column, value)?;
        }
        Ok(())
    }

    /// Opens or creates a database using conservative writer defaults.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(root, DatabaseOptions::default())
    }

    /// Opens or creates a database with explicit Tantivy writer and merge settings.
    pub fn open_with_options(root: impl AsRef<Path>, options: DatabaseOptions) -> Result<Self> {
        ensure!(
            options.writer_threads > 0,
            "writer_threads must be positive"
        );
        ensure!(
            options.writer_memory_bytes / options.writer_threads >= 15_000_000,
            "writer_memory_bytes must provide at least 15 MB per writer thread"
        );
        ensure!(
            (0.0..=1.0).contains(&options.deleted_docs_merge_ratio)
                && options.deleted_docs_merge_ratio > 0.0,
            "deleted_docs_merge_ratio must be in (0, 1]"
        );
        ensure!(
            options.min_merge_segments > 0,
            "min_merge_segments must be positive"
        );
        ensure!(
            options.sqlite_wal_autocheckpoint_pages > 0,
            "sqlite_wal_autocheckpoint_pages must be positive"
        );
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("indexes"))?;
        let conn = Connection::open(root.join("data.sqlite3"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(
            None,
            "synchronous",
            options.sqlite_synchronous.pragma_value(),
        )?;
        conn.pragma_update(
            None,
            "wal_autocheckpoint",
            options.sqlite_wal_autocheckpoint_pages,
        )?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(CATALOG_SQL)?;
        let mut db = Self {
            root,
            conn,
            indexes: HashMap::new(),
            deferred_outbox_ids: HashSet::new(),
            staged_outbox_ids: HashSet::new(),
            options,
        };
        db.recover()?;
        Ok(db)
    }

    /// Inserts schema-ordered JSON rows and makes them immediately searchable.
    pub fn bulk_insert_json(&mut self, table: &str, rows: &[Vec<Value>]) -> Result<usize> {
        let inserted = self.bulk_insert_json_deferred(table, rows)?;
        self.flush()?;
        Ok(inserted)
    }

    /// Inserts schema-ordered JSON rows while deferring the Tantivy commit.
    pub fn bulk_insert_json_deferred(&mut self, table: &str, rows: &[Vec<Value>]) -> Result<usize> {
        self.bulk_write_json_deferred(table, rows, false)
    }

    /// Upserts schema-ordered JSON rows while deferring the Tantivy commit.
    pub fn bulk_upsert_json_deferred(&mut self, table: &str, rows: &[Vec<Value>]) -> Result<usize> {
        self.bulk_write_json_deferred(table, rows, true)
    }

    fn bulk_write_json_deferred(
        &mut self,
        table: &str,
        rows: &[Vec<Value>],
        upsert: bool,
    ) -> Result<usize> {
        self.recover()?;
        if rows.is_empty() {
            return Ok(0);
        }
        let def = self.table(table)?;
        let mut converted_rows = Vec::with_capacity(rows.len());
        let mut recovery_operations = Vec::with_capacity(rows.len());
        let primary_key = primary_key_index(&def);
        for (row_index, values) in rows.iter().enumerate() {
            ensure!(
                values.len() == def.columns.len(),
                "bulk row {} has {} values for {} columns",
                row_index + 1,
                values.len(),
                def.columns.len()
            );
            let row = def
                .columns
                .iter()
                .zip(values)
                .map(|(column, value)| json_to_row_value(column, value))
                .collect::<Result<Vec<_>>>()?;
            recovery_operations.push(RowOperation::Refresh {
                key: row[primary_key].clone(),
            });
            converted_rows.push(row);
        }
        let tx = self.conn.transaction()?;
        if upsert {
            bulk_upsert_rows(&tx, &def, &converted_rows)?;
        } else {
            bulk_insert_rows(&tx, &def, &converted_rows)?;
        }
        tx.execute(
            "INSERT INTO __aq_outbox(table_name, operations_json) VALUES (?1, ?2)",
            params![def.name, serde_json::to_string(&recovery_operations)?],
        )?;
        let outbox_id = tx.last_insert_rowid();
        tx.commit()?;
        let operations = converted_rows
            .into_iter()
            .map(|row| RowOperation::Upsert { row })
            .collect::<Vec<_>>();
        self.stage_operations(&def, &operations)
            .context("failed to stage bulk rows in Tantivy")?;
        self.deferred_outbox_ids.insert(outbox_id);
        self.staged_outbox_ids.insert(outbox_id);
        Ok(rows.len())
    }

    /// Publishes all durable outbox mutations to Tantivy and reloads affected readers.
    pub fn flush(&mut self) -> Result<()> {
        let ids = {
            let mut stmt = self
                .conn
                .prepare("SELECT id FROM __aq_outbox ORDER BY id")?;
            stmt.query_map([], |row| row.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        self.apply_outboxes(&ids)?;
        for id in ids {
            self.deferred_outbox_ids.remove(&id);
            self.staged_outbox_ids.remove(&id);
        }
        Ok(())
    }

    /// Returns the durable catalog definition for a table.
    pub fn table(&self, name: &str) -> Result<TableDef> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT schema_json FROM __aq_tables WHERE name = ?1 COLLATE NOCASE",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(json) = json {
            return Ok(serde_json::from_str(&json)?);
        }
        self.tables()?
            .into_iter()
            .find(|table| {
                table
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| anyhow!("unknown table: {name}"))
    }

    /// Returns all durable table definitions ordered by name.
    pub fn tables(&self) -> Result<Vec<TableDef>> {
        let mut statement = self
            .conn
            .prepare("SELECT schema_json FROM __aq_tables ORDER BY name COLLATE NOCASE")?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|json| Ok(serde_json::from_str(&json?)?))
            .collect()
    }
}
