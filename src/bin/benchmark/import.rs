use super::*;
use frankensteindb::{Aggregate, GeoDistanceMode, Projection, ReadRequest, Sort};
use rayon::prelude::*;

pub(crate) fn existing_benchmark_state(
    database: &mut Database,
) -> Result<(usize, i64, Vec<Value>)> {
    let count = database.read(ReadRequest {
        table: "sutartys".into(),
        projection: vec![Projection::Aggregate {
            function: Aggregate::Count,
            column: None,
            alias: "rows".into(),
        }],
        filter: None,
        group_by: vec![],
        order_by: vec![],
        limit: 1,
        offset: 0,
        search_after: None,
        min_score: None,
    })?;
    let rows = count
        .rows
        .first()
        .and_then(|row| row.first())
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow::anyhow!("invalid COUNT(*) result"))? as usize;
    ensure!(rows > 0, "existing benchmark table is empty");

    let mut seed = database
        .read(ReadRequest {
            table: "sutartys".into(),
            projection: vec![],
            filter: None,
            group_by: vec![],
            order_by: vec![Sort {
                column: "unikalusId".into(),
                json_path: None,
                json_type: None,
                descending: false,
                geo_distance_from: None,
                geo_distance_mode: GeoDistanceMode::Min,
            }],
            limit: 1,
            offset: 0,
            search_after: None,
            min_score: None,
        })?
        .rows;
    let row = seed
        .pop()
        .ok_or_else(|| anyhow::anyhow!("existing benchmark table has no seed row"))?;
    let id = row
        .first()
        .and_then(Value::as_i64)
        .ok_or_else(|| anyhow::anyhow!("canonical unikalusId is not an integer"))?;
    Ok((rows, id, row))
}

pub(crate) fn ingest(
    database: &mut Database,
    dataset: &Path,
    source_bytes: u64,
    batch_size: usize,
    flush_rows: usize,
    import_threads: usize,
    progress: &ProgressReporter,
) -> Result<(usize, i64, Vec<Value>, Duration)> {
    let started = Instant::now();
    let mut rows = 0;
    let mut bytes_read = 0_u64;
    let mut first_id = None;
    let mut first_row = None;
    let mut rows_at_last_flush = 0;
    let mut timings = IngestionTimings::default();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let dataset = dataset.to_owned();
    let producer = std::thread::Builder::new()
        .name("aq-import-producer".into())
        .spawn(move || produce_batches(dataset, batch_size, import_threads, sender))?;

    let database_result = (|| -> Result<()> {
        loop {
            let waiting = Instant::now();
            let Ok(parsed) = receiver.recv() else {
                timings.input_wait += waiting.elapsed();
                break;
            };
            timings.input_wait += waiting.elapsed();
            bytes_read = parsed.bytes_read;
            let values = &parsed.rows[0];
            if first_id.is_none() {
                first_id = values[0].as_i64();
            }
            if first_row.is_none() {
                first_row = Some(values.clone());
            }
            rows += parsed.rows.len();
            let staging = Instant::now();
            insert_batch(database, &parsed.rows)?;
            timings.staging += staging.elapsed();
            if rows - rows_at_last_flush >= flush_rows {
                let flushing = Instant::now();
                database.flush()?;
                timings.flushing += flushing.elapsed();
                rows_at_last_flush = rows;
            }
            progress.ingestion(bytes_read, source_bytes, rows, started.elapsed(), &timings);
        }
        Ok(())
    })();
    drop(receiver);
    let producer_result = producer
        .join()
        .map_err(|_| anyhow::anyhow!("JSON import producer panicked"))?;
    database_result?;
    producer_result?;

    progress.ingestion_commit_started(source_bytes, rows, started.elapsed(), &timings);
    let flushing = Instant::now();
    database.flush()?;
    timings.flushing += flushing.elapsed();
    progress.ingestion_finished(rows, started.elapsed(), &timings);
    Ok((
        rows,
        first_id.ok_or_else(|| anyhow::anyhow!("dataset is empty"))?,
        first_row.ok_or_else(|| anyhow::anyhow!("dataset is empty"))?,
        started.elapsed(),
    ))
}

struct ParsedBatch {
    rows: Vec<Vec<Value>>,
    bytes_read: u64,
}

fn produce_batches(
    dataset: PathBuf,
    batch_size: usize,
    import_threads: usize,
    sender: std::sync::mpsc::SyncSender<ParsedBatch>,
) -> Result<()> {
    let file = File::open(dataset)?;
    let mut reader = BufReader::with_capacity(4 * 1024 * 1024, file);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(import_threads)
        .thread_name(|index| format!("aq-json-{index}"))
        .build()?;
    let mut line_number = 0;
    let mut bytes_read = 0_u64;
    loop {
        let lines = read_line_batch(&mut reader, batch_size, &mut line_number, &mut bytes_read)?;
        if lines.is_empty() {
            return Ok(());
        }
        let rows = pool.install(|| {
            lines
                .into_par_iter()
                .map(parse_canonical_line)
                .collect::<Result<Vec<_>>>()
        })?;
        if sender.send(ParsedBatch { rows, bytes_read }).is_err() {
            return Ok(());
        }
    }
}

fn read_line_batch(
    reader: &mut impl BufRead,
    batch_size: usize,
    line_number: &mut usize,
    bytes_read: &mut u64,
) -> Result<Vec<(usize, String)>> {
    let mut lines = Vec::with_capacity(batch_size);
    while lines.len() < batch_size {
        let mut line = String::new();
        let line_bytes = reader.read_line(&mut line)?;
        if line_bytes == 0 {
            break;
        }
        *line_number += 1;
        *bytes_read += line_bytes as u64;
        lines.push((*line_number, line));
    }
    Ok(lines)
}

fn parse_canonical_line((line_number, line): (usize, String)) -> Result<Vec<Value>> {
    let record: Value = serde_json::from_str(&line)
        .with_context(|| format!("invalid JSON on line {line_number}"))?;
    canonical_contract_row(&record)
        .with_context(|| format!("invalid canonical contract on line {line_number}"))
}

pub(crate) fn insert_batch(database: &mut Database, rows: &[Vec<Value>]) -> Result<()> {
    database.bulk_insert_json_deferred("sutartys", rows)?;
    Ok(())
}
