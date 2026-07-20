use std::collections::BTreeMap;

use frankensteindb::{Comparison, Filter, Mutation, ReadRequest};
use serde_json::json;

use super::*;

#[derive(Debug, Serialize)]
pub(crate) struct Measurement {
    pub(crate) name: String,
    pub(crate) iterations: usize,
    pub(crate) result_rows: usize,
    pub(crate) min_ms: f64,
    pub(crate) median_ms: f64,
    p95_ms: f64,
    pub(crate) max_ms: f64,
}

pub(crate) fn measure_read(
    database: &mut Database,
    name: &str,
    request: &ReadRequest,
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    database.read(request.clone())?;
    let mut samples = Vec::with_capacity(iterations);
    let mut result_rows = 0;
    let mut last_result = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let result = std::hint::black_box(database.read(std::hint::black_box(request.clone()))?);
        samples.push(started.elapsed());
        result_rows = result.rows.len();
        last_result = Some(result);
    }
    let measurement = measurement(name, result_rows, samples);
    if let (Some(capture), Some(result)) = (capture, last_result) {
        capture.record(name, read_sql(request), &result, &measurement)?;
    }
    Ok(measurement)
}

pub(crate) fn measure_updates(
    database: &mut Database,
    id: i64,
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    let mut samples = Vec::with_capacity(iterations);
    let mut last = None;
    for iteration in 0..iterations {
        let marker = if iteration % 2 == 0 {
            "benchmark-a"
        } else {
            "benchmark-b"
        };
        let mutation = update_by("unikalusId", json!(id), "pirkimoNumeris", json!(marker));
        let started = Instant::now();
        let result = std::hint::black_box(database.mutate_typed(mutation.clone())?);
        samples.push(started.elapsed());
        last = Some((mutation, result));
    }
    captured_mutation("single_row_update", samples, last, capture)
}

pub(crate) fn measure_selective_updates(
    database: &mut Database,
    id: i64,
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    let mut samples = Vec::with_capacity(iterations);
    let mut last = None;
    for iteration in 0..iterations {
        let marker = format!("benchmark-selective-update-{iteration}");
        database.mutate_typed(update_by(
            "unikalusId",
            json!(id),
            "pirkimoNumeris",
            json!(marker),
        ))?;
        let mutation = update_by(
            "pirkimoNumeris",
            json!(marker),
            "pirkimoNumeris",
            json!("benchmark-selective-updated"),
        );
        let started = Instant::now();
        let result = std::hint::black_box(database.mutate_typed(mutation.clone())?);
        samples.push(started.elapsed());
        last = Some((mutation, result));
    }
    captured_mutation("selective_non_pk_update", samples, last, capture)
}

pub(crate) fn measure_selective_deletes(
    database: &mut Database,
    seed_row: &[Value],
    iterations: usize,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    const ID_INDEX: usize = 0;
    const NUMBER_INDEX: usize = 10;
    let mut samples = Vec::with_capacity(iterations);
    let mut last = None;
    for iteration in 0..iterations {
        let marker = format!("benchmark-selective-delete-{iteration}");
        let mut row = seed_row.to_vec();
        row[ID_INDEX] = json!(-9_000_000_000_i64 - iteration as i64);
        row[NUMBER_INDEX] = json!(marker);
        database.bulk_insert_json("sutartys", &[row])?;
        let mutation = Mutation::Delete {
            table: "sutartys".into(),
            filter: equal("pirkimoNumeris", json!(marker)),
        };
        let started = Instant::now();
        let result = std::hint::black_box(database.mutate_typed(mutation.clone())?);
        samples.push(started.elapsed());
        last = Some((mutation, result));
    }
    captured_mutation("selective_non_pk_delete", samples, last, capture)
}

fn captured_mutation(
    name: &str,
    samples: Vec<Duration>,
    last: Option<(Mutation, frankensteindb::QueryResult)>,
    capture: Option<&mut BenchmarkCapture>,
) -> Result<Measurement> {
    let measurement = measurement(name, 1, samples);
    if let (Some(capture), Some((mutation, result))) = (capture, last) {
        capture.record(name, mutation_sql(&mutation), &result, &measurement)?;
    }
    Ok(measurement)
}

fn update_by(filter_column: &str, filter_value: Value, column: &str, value: Value) -> Mutation {
    Mutation::Update {
        table: "sutartys".into(),
        values: BTreeMap::from([(column.into(), value)]),
        filter: equal(filter_column, filter_value),
    }
}

fn equal(column: &str, value: Value) -> Filter {
    Filter::Compare {
        column: column.into(),
        operator: Comparison::Equal,
        value,
    }
}

pub(crate) fn measurement(
    name: &str,
    result_rows: usize,
    mut samples: Vec<Duration>,
) -> Measurement {
    samples.sort_unstable();
    let milliseconds = |duration: Duration| duration.as_secs_f64() * 1000.0;
    let p95_index = ((samples.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    Measurement {
        name: name.into(),
        iterations: samples.len(),
        result_rows,
        min_ms: milliseconds(samples[0]),
        median_ms: milliseconds(samples[samples.len() / 2]),
        p95_ms: milliseconds(samples[p95_index]),
        max_ms: milliseconds(*samples.last().unwrap()),
    }
}
