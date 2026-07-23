use std::fs::{self, File};
use std::io::{BufRead, BufReader, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, ValueEnum};
use frankensteindb::{
    Database, DatabaseOptions, DocumentCompression, DocumentStore, SearchOptions,
    SqliteSynchronous, canonical_contract_row, canonical_contract_table,
};
use serde::Serialize;
use serde_json::Value;

#[path = "../global_allocator.rs"]
mod global_allocator;

#[path = "benchmark/aggregation_suite.rs"]
mod aggregation_suite;
#[cfg(test)]
#[path = "benchmark/tests.rs"]
mod benchmark_tests;
#[path = "benchmark/capture.rs"]
mod capture;
#[path = "benchmark/import.rs"]
mod import;
#[path = "benchmark/measure.rs"]
mod measure;
#[path = "benchmark/progress.rs"]
mod progress;
#[path = "benchmark/sql_aggregation.rs"]
mod sql_aggregation;
#[path = "benchmark/sql_display.rs"]
mod sql_display;
#[path = "benchmark/suite.rs"]
mod suite;

use aggregation_suite::*;
use capture::*;
use import::*;
use measure::*;
use progress::*;
use sql_display::*;
use suite::*;

const IGNORED_COLUMNS: &[&str] = &["dokumentai"];
const FLATTENED_COLUMNS: &[&str] = &[
    "pirmoTiekejoKodas + papildomiTiekejai[].kodas -> tiekejuKodai",
    "pirmoTiekejoPavadinimas + papildomiTiekejai[].pavadinimas -> tiekejuPavadinimai",
    "bvpzKodas + papildomiBvpzKodai[] -> bvpzKodai",
];

#[derive(Debug, Parser)]
#[command(about = "Benchmark FrankensteinDB with canonical VPM contracts")]
struct Args {
    #[arg(long, default_value = "sutartysCanonical.jsonl")]
    dataset: PathBuf,

    #[arg(long, default_value = "target/sutartys-benchmark")]
    database: PathBuf,

    /// Remove an existing benchmark database first.
    #[arg(long)]
    reset: bool,

    /// Reuse an already imported database and run only the measured benchmark cases.
    #[arg(long, alias = "reuse", conflicts_with = "reset")]
    skip_import: bool,

    #[arg(long, default_value_t = 20_000)]
    batch_size: usize,

    /// Commit Tantivy and clear the durable outbox after this many ingested rows.
    #[arg(long, default_value_t = 1_000_000)]
    flush_rows: usize,

    /// Threads used to parse and flatten input JSON records.
    #[arg(long, default_value_t = default_import_threads())]
    import_threads: usize,

    /// Tantivy indexing worker threads.
    #[arg(long, default_value_t = default_index_threads())]
    index_threads: usize,

    /// Total memory in MiB shared by Tantivy indexing workers.
    #[arg(long, default_value_t = 512)]
    index_memory_mib: usize,

    /// SQLite WAL sync policy; NORMAL is appropriate for a repeatable benchmark import.
    #[arg(long, value_enum, default_value_t = BenchmarkSynchronous::Normal)]
    sqlite_synchronous: BenchmarkSynchronous,

    /// Tantivy stored-document compression used for a newly imported index.
    #[arg(long, value_enum, default_value_t = BenchmarkCompression::None)]
    compression: BenchmarkCompression,

    /// Zstd compression level; valid only with `--compression zstd`.
    #[arg(long)]
    zstd_level: Option<i32>,

    /// Uncompressed bytes per Tantivy document-store block.
    #[arg(long, default_value_t = 16_384)]
    docstore_block_size: usize,

    /// Run Tantivy document-store compression on its dedicated thread.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    docstore_compression_thread: bool,

    #[arg(long, default_value_t = 2)]
    iterations: usize,

    /// CPU workers shared by benchmarked searches. Zero uses available system parallelism.
    #[arg(long, default_value_t = 0)]
    search_threads: usize,

    /// Disable progress messages on stderr.
    #[arg(long)]
    no_progress: bool,

    /// Save benchmark requests, representative results, and timings; defaults to results.txt.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "results.txt")]
    save_results: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    dataset: String,
    rows: usize,
    source_bytes: u64,
    ignored_columns: &'static [&'static str],
    flattened_columns: &'static [&'static str],
    batch_size: usize,
    flush_rows: usize,
    import_threads: usize,
    index_threads: usize,
    index_memory_mib: usize,
    search_threads: usize,
    sqlite_synchronous: BenchmarkSynchronous,
    document_store: DocumentStore,
    import_skipped: bool,
    ingestion_seconds: Option<f64>,
    ingestion_rows_per_second: Option<f64>,
    sqlite_bytes: u64,
    tantivy_bytes: u64,
    benchmarks: Vec<Measurement>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let progress = ProgressReporter::new(!args.no_progress);
    ensure!(
        args.batch_size > 0,
        "--batch-size must be greater than zero"
    );
    ensure!(
        args.flush_rows > 0,
        "--flush-rows must be greater than zero"
    );
    ensure!(
        args.iterations > 0,
        "--iterations must be greater than zero"
    );
    ensure!(
        args.import_threads > 0,
        "--import-threads must be greater than zero"
    );
    ensure!(
        args.index_threads > 0,
        "--index-threads must be greater than zero"
    );
    ensure!(
        args.index_memory_mib > 0,
        "--index-memory-mib must be greater than zero"
    );
    validate_compression_args(&args)?;
    if !args.skip_import {
        ensure!(
            args.dataset.is_file(),
            "dataset not found: {}",
            args.dataset.display()
        );
    }
    if args.skip_import {
        ensure!(
            args.database.is_dir(),
            "database does not exist: {}; import it first without --skip-import",
            args.database.display()
        );
    } else if args.database.exists() {
        if !args.reset {
            bail!(
                "database already exists; pass --reset to replace it or --skip-import to reuse {}",
                args.database.display()
            );
        }
        progress.message(format!("resetting {}", args.database.display()));
        fs::remove_dir_all(&args.database)?;
    }

    let source_bytes = fs::metadata(&args.dataset)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    progress.message(format!("opening {}", args.database.display()));
    let mut database = Database::open_with_options(
        &args.database,
        DatabaseOptions {
            writer_memory_bytes: args.index_memory_mib * 1024 * 1024,
            writer_threads: args.index_threads,
            sqlite_wal_autocheckpoint_pages: 65_536,
            sqlite_synchronous: args.sqlite_synchronous.into(),
            ..DatabaseOptions::default()
        },
    )?;
    let (rows, first_id, first_row, ingestion_elapsed) = if args.skip_import {
        progress.message("skipping import; loading benchmark seed from the existing Tantivy index");
        let (rows, first_id, first_row) = existing_benchmark_state(&mut database)
            .context("existing database does not contain the canonical benchmark table")?;
        progress.message(format!("reusing {rows} indexed row(s)"));
        (rows, first_id, first_row, None)
    } else {
        let mut table = canonical_contract_table();
        table.document_store = benchmark_document_store(&args);
        database.create_table_def(table)?;
        progress.message(format!(
            "ingesting {} ({:.1} MiB, batches of {}, flush every {}, {} JSON workers, {} Tantivy workers/{} MiB, SQLite {:?}, document store {:?})",
            args.dataset.display(),
            source_bytes as f64 / 1024.0 / 1024.0,
            args.batch_size,
            args.flush_rows,
            args.import_threads,
            args.index_threads,
            args.index_memory_mib,
            args.sqlite_synchronous,
            args.compression,
        ));
        let (rows, first_id, first_row, elapsed) = ingest(
            &mut database,
            &args.dataset,
            source_bytes,
            args.batch_size,
            args.flush_rows,
            args.import_threads,
            &progress,
        )?;
        (rows, first_id, first_row, Some(elapsed))
    };

    let mut capture = args
        .save_results
        .as_ref()
        .map(|_| BenchmarkCapture::default());
    let benchmarks = run_benchmark_suite(
        &mut database,
        first_id,
        &first_row,
        args.iterations,
        args.search_threads,
        &progress,
        capture.as_mut(),
    )?;

    let sqlite_bytes = file_size(&args.database.join("data.sqlite3"));
    let tantivy_bytes = directory_size(&args.database.join("indexes"))?;
    let ingestion_seconds = ingestion_elapsed.map(|elapsed| elapsed.as_secs_f64());
    let document_store = database.table("sutartys")?.document_store;
    let report = BenchmarkReport {
        dataset: args.dataset.display().to_string(),
        rows,
        source_bytes,
        ignored_columns: IGNORED_COLUMNS,
        flattened_columns: FLATTENED_COLUMNS,
        batch_size: args.batch_size,
        flush_rows: args.flush_rows,
        import_threads: args.import_threads,
        index_threads: args.index_threads,
        index_memory_mib: args.index_memory_mib,
        search_threads: args.search_threads,
        sqlite_synchronous: args.sqlite_synchronous,
        document_store,
        import_skipped: args.skip_import,
        ingestion_seconds,
        ingestion_rows_per_second: ingestion_seconds.map(|seconds| rows as f64 / seconds),
        sqlite_bytes,
        tantivy_bytes,
        benchmarks,
    };
    if let (Some(path), Some(capture)) = (&args.save_results, capture) {
        capture.save(path)?;
        progress.message(format!(
            "saved benchmark queries and results to {}",
            path.display()
        ));
    }
    progress.message("writing JSON report");
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[derive(Debug, Clone, Copy, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum BenchmarkSynchronous {
    Full,
    Normal,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
enum BenchmarkCompression {
    None,
    Lz4,
    Zstd,
}

impl From<BenchmarkCompression> for DocumentCompression {
    fn from(value: BenchmarkCompression) -> Self {
        match value {
            BenchmarkCompression::None => Self::None,
            BenchmarkCompression::Lz4 => Self::Lz4,
            BenchmarkCompression::Zstd => Self::Zstd,
        }
    }
}

fn benchmark_document_store(args: &Args) -> DocumentStore {
    DocumentStore {
        compression: args.compression.into(),
        zstd_level: args.zstd_level,
        block_size: args.docstore_block_size,
        dedicated_thread: args.docstore_compression_thread,
    }
}

fn validate_compression_args(args: &Args) -> Result<()> {
    ensure!(
        args.compression == BenchmarkCompression::Zstd || args.zstd_level.is_none(),
        "--zstd-level requires --compression zstd"
    );
    ensure!(
        (1_024..=16 * 1024 * 1024).contains(&args.docstore_block_size),
        "--docstore-block-size must be between 1024 and 16777216"
    );
    if let Some(level) = args.zstd_level {
        ensure!(
            zstd::compression_level_range().contains(&level),
            "--zstd-level is outside the supported range"
        );
    }
    Ok(())
}

impl From<BenchmarkSynchronous> for SqliteSynchronous {
    fn from(value: BenchmarkSynchronous) -> Self {
        match value {
            BenchmarkSynchronous::Full => Self::Full,
            BenchmarkSynchronous::Normal => Self::Normal,
            BenchmarkSynchronous::Off => Self::Off,
        }
    }
}

fn available_threads() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

fn default_index_threads() -> usize {
    available_threads().min(4)
}

fn default_import_threads() -> usize {
    available_threads()
        .saturating_sub(default_index_threads())
        .max(1)
}

fn file_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut size = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        size += if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        };
    }
    Ok(size)
}
