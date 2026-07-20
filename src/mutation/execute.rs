use super::*;

/// Applies validated typed assignments through SQLite's parameterized write path.
pub(crate) fn update_rows_by_key(
    tx: &Transaction<'_>,
    def: &TableDef,
    assignments: &[(String, RowValue)],
    keys: &[RowValue],
) -> Result<usize> {
    if keys.is_empty() {
        return Ok(0);
    }
    let primary_key = &def.columns[primary_key_index(def)];
    let statement = format!(
        "UPDATE {} SET {} WHERE {} = ?{}",
        quote_ident(&def.name),
        assignments
            .iter()
            .enumerate()
            .map(|(index, (name, _))| format!("{} = ?{}", quote_ident(name), index + 1))
            .collect::<Vec<_>>()
            .join(", "),
        quote_ident(&primary_key.name),
        assignments.len() + 1
    );
    let mut statement = tx.prepare_cached(&statement)?;
    let mut changed = 0;
    for key in keys {
        let values = assignments
            .iter()
            .map(|(_, value)| sqlite_value(value))
            .chain(std::iter::once(sqlite_value(key)))
            .collect::<Vec<_>>();
        changed += statement.execute(rusqlite::params_from_iter(values))?;
    }
    Ok(changed)
}

/// Deletes validated keys through SQLite's parameterized write path.
pub(crate) fn delete_rows_by_key(
    tx: &Transaction<'_>,
    def: &TableDef,
    keys: &[RowValue],
) -> Result<usize> {
    if keys.is_empty() {
        return Ok(0);
    }
    let primary_key = &def.columns[primary_key_index(def)];
    let statement = format!(
        "DELETE FROM {} WHERE {} = ?1",
        quote_ident(&def.name),
        quote_ident(&primary_key.name)
    );
    let mut statement = tx.prepare_cached(&statement)?;
    let mut changed = 0;
    for key in keys {
        changed += statement.execute([sqlite_value(key)])?;
    }
    Ok(changed)
}
