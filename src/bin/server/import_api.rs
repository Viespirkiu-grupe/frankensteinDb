use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path as FilePath;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use frankensteindb::{Database, TableDef};
use futures_util::StreamExt;
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;

use crate::api_types::DataResponse;
use crate::state::{AppState, WebError, WebResult};

#[derive(Debug, Deserialize)]
pub(crate) struct ImportOptions {
    #[serde(default = "default_batch_size")]
    batch_size: usize,
    #[serde(default)]
    on_error: ErrorMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ErrorMode {
    #[default]
    Abort,
    Skip,
}

pub(crate) async fn create(
    State(state): State<AppState>,
    Path(table): Path<String>,
    Query(options): Query<ImportOptions>,
    headers: HeaderMap,
    body: Body,
) -> WebResult<(StatusCode, Json<DataResponse<crate::jobs::Job>>)> {
    state.ensure_table_writable(&table)?;
    if !(1..=50_000).contains(&options.batch_size) {
        return Err(WebError::bad_request(
            "batch_size must be between 1 and 50000",
        ));
    }
    let encoding = content_encoding(&headers)?;
    let upload = state.jobs.artifact_path(
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        "upload",
    );
    if let Err(error) = stream_to_file(body, &upload).await {
        let _ = tokio::fs::remove_file(&upload).await;
        return Err(error);
    }
    let path = upload.clone();
    let job = state
        .start_job("import", Some(table.clone()), move |database, id, jobs| {
            let result = import_file(
                database,
                &table,
                &path,
                encoding,
                options.batch_size,
                options.on_error,
                |progress| {
                    anyhow::ensure!(!jobs.is_cancelled(id)?, "import cancelled");
                    jobs.progress(id, progress)
                },
            );
            let _ = std::fs::remove_file(&path);
            result
        })
        .map_err(WebError::from)?;
    Ok((StatusCode::ACCEPTED, Json(DataResponse::new(job))))
}

async fn stream_to_file(body: Body, path: &FilePath) -> WebResult<()> {
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| WebError::internal(error.to_string()))?;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk.map_err(|error| WebError::bad_request(error.to_string()))?)
            .await
            .map_err(|error| WebError::internal(error.to_string()))?;
    }
    file.flush()
        .await
        .map_err(|error| WebError::internal(error.to_string()))
}

#[derive(Clone, Copy)]
enum Encoding {
    Identity,
    Gzip,
    Zstd,
}

fn content_encoding(headers: &HeaderMap) -> WebResult<Encoding> {
    match headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("identity")
    {
        "identity" => Ok(Encoding::Identity),
        "gzip" => Ok(Encoding::Gzip),
        "zstd" => Ok(Encoding::Zstd),
        value => Err(WebError::bad_request(format!(
            "unsupported Content-Encoding: {value}"
        ))),
    }
}

fn import_file(
    database: &mut Database,
    table: &str,
    path: &FilePath,
    encoding: Encoding,
    batch_size: usize,
    error_mode: ErrorMode,
    progress: impl Fn(f64) -> anyhow::Result<()>,
) -> anyhow::Result<Value> {
    let file = File::open(path)?;
    let bytes = file.metadata()?.len().max(1);
    let consumed = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        input: file,
        consumed: consumed.clone(),
    };
    let input: Box<dyn Read> = match encoding {
        Encoding::Identity => Box::new(counting),
        Encoding::Gzip => Box::new(flate2::read::GzDecoder::new(counting)),
        Encoding::Zstd => Box::new(zstd::stream::read::Decoder::new(counting)?),
    };
    let def = database.table(table)?;
    let mut reader = BufReader::new(input);
    let mut accepted = 0_usize;
    let mut rejected = 0_usize;
    loop {
        let mut lines = Vec::with_capacity(batch_size);
        while lines.len() < batch_size {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            lines.push(line);
        }
        if lines.is_empty() {
            break;
        }
        let parsed = lines
            .into_par_iter()
            .map(|line| parse_row(&def, &line))
            .collect::<Vec<_>>();
        let mut batch = Vec::with_capacity(parsed.len());
        for parsed_row in parsed {
            match parsed_row.and_then(|row| {
                if matches!(error_mode, ErrorMode::Skip) {
                    database.validate_json_row(table, &row)?;
                }
                Ok(row)
            }) {
                Ok(row) => batch.push(row),
                Err(error) if matches!(error_mode, ErrorMode::Skip) => {
                    rejected += 1;
                    if rejected <= 20 {
                        eprintln!("import rejected row: {error:#}");
                    }
                }
                Err(error) => return Err(error),
            }
        }
        accepted += database.bulk_upsert_json_deferred(table, &batch)?;
        progress((consumed.load(Ordering::Relaxed) as f64 / bytes as f64).min(0.99))?;
    }
    database.flush()?;
    progress(1.0)?;
    Ok(serde_json::json!({"processed": accepted, "rejected": rejected}))
}

struct CountingReader<R> {
    input: R,
    consumed: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.input.read(buffer)?;
        self.consumed.fetch_add(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

fn parse_row(def: &TableDef, line: &str) -> anyhow::Result<Vec<Value>> {
    let object = serde_json::from_str::<Value>(line)?;
    let object = object
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("NDJSON row must be an object"))?;
    let known = def
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect::<HashSet<_>>();
    if let Some(unknown) = object.keys().find(|name| !known.contains(name.as_str())) {
        anyhow::bail!("unknown column: {unknown}");
    }
    Ok(def
        .columns
        .iter()
        .map(|column| object.get(&column.name).cloned().unwrap_or(Value::Null))
        .collect())
}

const fn default_batch_size() -> usize {
    5_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use frankensteindb::{Analyzer, ColumnDef, ColumnType, TableDef};

    #[test]
    fn ndjson_import_upserts_and_reports_rejections() {
        let directory = tempfile::tempdir().unwrap();
        let mut database = Database::open(directory.path()).unwrap();
        database
            .create_table_def(TableDef {
                name: "items".into(),
                aliases: vec![],
                document_store: Default::default(),
                columns: vec![
                    ColumnDef {
                        name: "id".into(),
                        data_type: ColumnType::Integer,
                        primary_key: true,
                        nullable: false,
                        analyzer: None,
                        compact_raw: false,
                        index: Default::default(),
                    },
                    ColumnDef {
                        name: "name".into(),
                        data_type: ColumnType::Text,
                        primary_key: false,
                        nullable: false,
                        analyzer: Some(Analyzer::Default),
                        compact_raw: false,
                        index: Default::default(),
                    },
                ],
            })
            .unwrap();
        let input = directory.path().join("rows.ndjson");
        std::fs::write(&input, "{\"id\":1,\"name\":\"first\"}\n{\"id\":1,\"name\":\"updated\"}\n{\"id\":\"bad\",\"name\":\"bad\"}\n").unwrap();
        let result = import_file(
            &mut database,
            "items",
            &input,
            Encoding::Identity,
            2,
            ErrorMode::Skip,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(result["processed"], 2);
        assert_eq!(result["rejected"], 1);
        let rows = database
            .read(frankensteindb::ReadRequest {
                table: "items".into(),
                projection: vec![],
                filter: None,
                group_by: vec![],
                order_by: vec![],
                limit: 10,
                offset: 0,
                search_after: None,
                min_score: None,
            })
            .unwrap();
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(rows.rows[0][1], "updated");
    }
}
