use super::*;

impl Database {
    /// Creates a SQLite table and its corresponding Tantivy index from typed metadata.
    pub fn create_table_def(&mut self, def: TableDef) -> Result<QueryResult> {
        self.flush()?;
        validate_table_def(&def)?;
        validate_alias_ownership(&self.tables()?, &def.name, &def.aliases)?;
        ensure!(
            self.table(&def.name).is_err(),
            "table already exists: {}",
            def.name
        );
        let tx = self.conn.transaction()?;
        tx.execute_batch(&sqlite_create_sql(&def))?;
        tx.execute(
            "INSERT INTO __aq_tables(name, schema_json, dirty) VALUES (?1, ?2, 1)",
            params![def.name, serde_json::to_string(&def)?],
        )?;
        tx.commit()?;
        self.rebuild_table(&def.name)?;
        Ok(QueryResult::message(format!("created table {}", def.name)))
    }

    /// Atomically replaces aliases without rebuilding the underlying Tantivy generation.
    pub fn set_table_aliases(&mut self, table: &str, aliases: Vec<String>) -> Result<TableDef> {
        let mut def = self.table(table)?;
        def.aliases = aliases;
        validate_table_def(&def)?;
        validate_alias_ownership(&self.tables()?, &def.name, &def.aliases)?;
        self.conn.execute(
            "UPDATE __aq_tables SET schema_json=?1 WHERE name=?2",
            params![serde_json::to_string(&def)?, def.name],
        )?;
        Ok(def)
    }

    /// Drops a table from SQLite, the catalog, and Tantivy.
    pub fn drop_table_named(&mut self, name: &str) -> Result<QueryResult> {
        self.flush()?;
        let name = self.table(name)?.name;
        let tx = self.conn.transaction()?;
        tx.execute_batch(&format!("DROP TABLE {}", quote_ident(&name)))?;
        tx.execute("DELETE FROM __aq_outbox WHERE table_name = ?1", [&name])?;
        tx.execute("DELETE FROM __aq_tables WHERE name = ?1", [&name])?;
        tx.commit()?;
        self.indexes.remove(&name);
        let path = self.index_path(&name);
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        Ok(QueryResult::message(format!("dropped table {name}")))
    }

    /// Rebuilds one Tantivy index from its authoritative SQLite rows.
    pub fn reindex_table(&mut self, name: &str) -> Result<QueryResult> {
        self.flush()?;
        let name = self.table(name)?.name;
        self.conn
            .execute("UPDATE __aq_tables SET dirty = 1 WHERE name = ?1", [&name])?;
        let count = self.rebuild_table(&name)?;
        Ok(QueryResult::message(format!(
            "reindexed {count} row(s) from {name}"
        )))
    }

    /// Forces all searchable Tantivy segments for a table into one segment.
    pub fn optimize_table(&mut self, name: &str) -> Result<QueryResult> {
        self.flush()?;
        let def = self.table(name)?;
        let options = self.options.clone();
        let handle = self.index_handle_mut(&def)?;
        let old_writer = handle.writer.take().expect("initialized index writer");
        old_writer.wait_merging_threads()?;
        handle.writer = Some(new_index_writer(&handle.index, &options)?);
        let segment_ids = handle.index.searchable_segment_ids()?;
        let before = segment_ids.len();
        if before > 1 {
            handle
                .writer
                .as_mut()
                .expect("initialized index writer")
                .merge(&segment_ids)
                .wait()?;
            handle.reader.reload()?;
        }
        Ok(QueryResult::message(format!(
            "optimized {} segment(s) in {}",
            before, def.name
        )))
    }

    pub(super) fn recover(&mut self) -> Result<()> {
        let names = {
            let mut stmt = self
                .conn
                .prepare("SELECT name FROM __aq_tables WHERE dirty = 1")?;
            stmt.query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for name in names {
            self.rebuild_table(&name)
                .with_context(|| format!("failed to recover search index for {name}"))?;
        }
        self.replay_outbox()?;
        Ok(())
    }
}

fn validate_alias_ownership(tables: &[TableDef], owner: &str, aliases: &[String]) -> Result<()> {
    for alias in aliases {
        ensure!(
            tables.iter().all(|table| {
                table.name.eq_ignore_ascii_case(owner)
                    || (!table.name.eq_ignore_ascii_case(alias)
                        && table
                            .aliases
                            .iter()
                            .all(|existing| !existing.eq_ignore_ascii_case(alias)))
            }),
            "table name or alias already exists: {alias}"
        );
    }
    Ok(())
}
