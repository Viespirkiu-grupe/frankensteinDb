use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use frankensteindb::{Database, Mutation, OptimizeOptions, ReadRequest, TableDef};
use serde::Serialize;

mod global_allocator;

#[derive(Debug, Parser)]
#[command(about = "Typed FrankensteinDB administration CLI")]
struct Args {
    /// Directory containing SQLite storage and Tantivy indexes.
    database: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a table from a JSON TableDef document.
    Create { document: PathBuf },
    /// Execute a typed JSON ReadRequest through Tantivy.
    Read { document: PathBuf },
    /// Apply a typed JSON Mutation.
    Mutate {
        document: PathBuf,
        #[arg(long)]
        deferred: bool,
    },
    /// Publish all deferred mutations to Tantivy.
    Flush,
    /// List typed table definitions.
    Tables,
    /// Remove a table and its index.
    Drop { table: String },
    /// Rebuild a Tantivy index from SQLite.
    Reindex { table: String },
    /// Merge a table's searchable segments.
    Optimize {
        table: String,
        /// Maximum number of searchable segments to retain.
        #[arg(long, default_value_t = 8)]
        target_segments: usize,
        /// Concurrent merge workers; zero selects up to four available CPUs.
        #[arg(long, default_value_t = 0)]
        merge_threads: usize,
    },
    /// Restore a portable backup. The server must be stopped.
    Restore {
        archive: PathBuf,
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Command::Restore { archive, force } = &args.command {
        frankensteindb::restore_backup(&args.database, archive, *force)?;
        print_json(&serde_json::json!({ "message": "backup restored" }))?;
        return Ok(());
    }
    let mut database = Database::open(&args.database)?;
    match args.command {
        Command::Create { document } => {
            let def: TableDef = read_json(document)?;
            print_json(&database.create_table_def(def)?)?;
        }
        Command::Read { document } => {
            let request: ReadRequest = read_json(document)?;
            print_json(&database.read(request)?)?;
        }
        Command::Mutate { document, deferred } => {
            let mutation: Mutation = read_json(document)?;
            let result = if deferred {
                database.mutate_typed_deferred(mutation)?
            } else {
                database.mutate_typed(mutation)?
            };
            print_json(&result)?;
        }
        Command::Flush => {
            database.flush()?;
            print_json(&serde_json::json!({ "message": "flushed" }))?;
        }
        Command::Tables => print_json(&database.tables()?)?,
        Command::Drop { table } => print_json(&database.drop_table_named(&table)?)?,
        Command::Reindex { table } => print_json(&database.reindex_table(&table)?)?,
        Command::Optimize {
            table,
            target_segments,
            merge_threads,
        } => print_json(&database.optimize_table_with_options(
            &table,
            OptimizeOptions {
                target_segments,
                merge_threads,
            },
        )?)?,
        Command::Restore { .. } => unreachable!(),
    }
    Ok(())
}

fn read_json<T>(path: PathBuf) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let document = if path.as_os_str() == "-" {
        let mut document = String::new();
        io::stdin().read_to_string(&mut document)?;
        document
    } else {
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?
    };
    serde_json::from_str(&document).context("invalid typed JSON document")
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
