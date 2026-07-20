use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use super::*;

const SAMPLE_ROWS: usize = 10;
const SAMPLE_ITEMS: usize = 10;
const MAX_HORIZONTAL_COLUMNS: usize = 6;
const MAX_CELL_CHARS: usize = 48;

/// Optional human-readable benchmark artifact written separately from stdout JSON.
#[derive(Debug, Default)]
pub(crate) struct BenchmarkCapture {
    benchmarks: Vec<CapturedBenchmark>,
}

#[derive(Debug)]
struct CapturedBenchmark {
    name: String,
    sql: String,
    result: Value,
    timing: Value,
}

impl BenchmarkCapture {
    pub(crate) fn record<R>(
        &mut self,
        name: &str,
        sql: String,
        result: &R,
        timing: &Measurement,
    ) -> Result<()>
    where
        R: Serialize,
    {
        self.benchmarks.push(CapturedBenchmark {
            name: name.into(),
            sql,
            result: serde_json::to_value(result)?,
            timing: serde_json::to_value(timing)?,
        });
        Ok(())
    }

    pub(crate) fn save(self, path: &Path) -> Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, self.render())?;
        Ok(())
    }

    fn render(&self) -> String {
        let mut output = String::from(
            "# FrankensteinDB benchmark query samples\n\n\
             SQL below is informational SQL-ish notation; execution uses the typed Tantivy API.\n\
             Results are representative samples from measured iterations.\n\n",
        );
        for (index, benchmark) in self.benchmarks.iter().enumerate() {
            let _ = writeln!(output, "## {}. {}\n", index + 1, benchmark.name);
            let _ = writeln!(output, "### Query\n\n```sql\n{}\n```\n", benchmark.sql);
            render_timing(&mut output, &benchmark.timing);
            output.push_str("\n### Sample result\n\n");
            render_result(&mut output, &benchmark.result);
            output.push_str("\n\n");
        }
        output
    }
}

fn render_timing(output: &mut String, timing: &Value) {
    let number = |name: &str| timing[name].as_f64().unwrap_or_default();
    output.push_str("### Timing\n\n");
    render_table(
        output,
        &[
            "Iterations",
            "Min (ms)",
            "Median (ms)",
            "P95 (ms)",
            "Max (ms)",
        ],
        &[vec![
            timing["iterations"]
                .as_u64()
                .unwrap_or_default()
                .to_string(),
            format!("{:.3}", number("min_ms")),
            format!("{:.3}", number("median_ms")),
            format!("{:.3}", number("p95_ms")),
            format!("{:.3}", number("max_ms")),
        ]],
    );
}

fn render_result(output: &mut String, result: &Value) {
    if result.get("columns").is_some() && result.get("rows").is_some() {
        render_query_result(output, result);
    } else {
        render_aggregation_result(output, result);
    }
}

fn render_aggregation_result(output: &mut String, result: &Value) {
    let Some(aggregations) = result.as_object() else {
        let _ = writeln!(output, "{}", display_scalar(result));
        return;
    };
    if aggregations.is_empty() {
        output.push_str("_No result values returned._\n");
        return;
    }
    for (name, value) in aggregations.iter().take(SAMPLE_ITEMS) {
        let _ = writeln!(output, "#### `{name}`\n");
        render_aggregation_value(output, value);
    }
    if aggregations.len() > SAMPLE_ITEMS {
        let _ = writeln!(
            output,
            "_{} additional aggregation(s) omitted._",
            aggregations.len() - SAMPLE_ITEMS
        );
    }
}

fn render_aggregation_value(output: &mut String, value: &Value) {
    let Some(object) = value.as_object() else {
        let _ = writeln!(output, "{}", display_scalar(value));
        return;
    };
    if let Some(buckets) = object.get("buckets") {
        render_buckets(output, buckets);
        let metadata = object
            .iter()
            .filter(|(name, _)| name.as_str() != "buckets")
            .map(|(name, value)| vec![name.clone(), display_scalar(value)])
            .collect::<Vec<_>>();
        if !metadata.is_empty() {
            output.push_str("Metadata:\n\n");
            render_table(output, &["Field", "Value"], &metadata);
        }
    } else {
        let rows = object
            .iter()
            .take(SAMPLE_ITEMS)
            .map(|(name, value)| vec![name.clone(), display_scalar(value)])
            .collect::<Vec<_>>();
        render_table(output, &["Field", "Value"], &rows);
    }
}

fn render_buckets(output: &mut String, buckets: &Value) {
    let rows = match buckets {
        Value::Array(values) => values.iter().take(SAMPLE_ITEMS).collect::<Vec<_>>(),
        Value::Object(values) => values.values().take(SAMPLE_ITEMS).collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    if rows.is_empty() {
        output.push_str("_No buckets returned._\n\n");
        return;
    }
    let headers = bucket_headers(&rows);
    let table_rows = rows
        .iter()
        .map(|bucket| {
            headers
                .iter()
                .map(|header| display_scalar(&bucket[header]))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let header_refs = headers.iter().map(String::as_str).collect::<Vec<_>>();
    render_table(output, &header_refs, &table_rows);
    let total = match buckets {
        Value::Array(values) => values.len(),
        Value::Object(values) => values.len(),
        _ => 0,
    };
    if total > SAMPLE_ITEMS {
        let _ = writeln!(
            output,
            "_{} additional bucket(s) omitted._\n",
            total - SAMPLE_ITEMS
        );
    }
}

fn bucket_headers(rows: &[&Value]) -> Vec<String> {
    let mut headers = Vec::new();
    for row in rows {
        if let Some(object) = row.as_object() {
            for name in object.keys() {
                if !headers.contains(name) {
                    headers.push(name.clone());
                }
            }
        }
    }
    headers
}

fn render_query_result(output: &mut String, result: &Value) {
    let columns = result["columns"]
        .as_array()
        .map(|values| values.iter().map(display_scalar).collect::<Vec<_>>())
        .unwrap_or_default();
    let rows = result["rows"].as_array().cloned().unwrap_or_default();
    let displayed_rows = rows
        .iter()
        .take(SAMPLE_ROWS)
        .map(|row| {
            row.as_array()
                .map(|values| values.iter().map(display_scalar).collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    if !columns.is_empty() && columns.len() <= MAX_HORIZONTAL_COLUMNS {
        let headers = columns.iter().map(String::as_str).collect::<Vec<_>>();
        render_table(output, &headers, &displayed_rows);
    } else if !columns.is_empty() {
        render_wide_rows(output, &columns, &displayed_rows);
    }
    if rows.len() > SAMPLE_ROWS {
        let _ = writeln!(
            output,
            "… {} more returned row(s) omitted",
            rows.len() - SAMPLE_ROWS
        );
    }
    if rows.is_empty() {
        let _ = write!(
            output,
            "{}",
            result["message"].as_str().unwrap_or("no rows")
        );
    }
}

fn render_wide_rows(output: &mut String, columns: &[String], rows: &[Vec<String>]) {
    for (index, row) in rows.iter().enumerate() {
        let _ = writeln!(output, "#### Row {}\n", index + 1);
        let fields = columns
            .iter()
            .enumerate()
            .map(|(column_index, column)| {
                vec![
                    column.clone(),
                    row.get(column_index).cloned().unwrap_or_default(),
                ]
            })
            .collect::<Vec<_>>();
        render_table(output, &["Field", "Value"], &fields);
    }
}

fn render_table(output: &mut String, headers: &[&str], rows: &[Vec<String>]) {
    if headers.is_empty() {
        return;
    }
    let widths = headers
        .iter()
        .enumerate()
        .map(|(index, header)| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|cell| display_width(&truncate_cell(cell)))
                .chain(std::iter::once(display_width(&truncate_cell(header))))
                .max()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    render_table_row(output, headers.iter().copied(), &widths);
    render_table_separator(output, &widths);
    for row in rows {
        render_table_row(output, row.iter().map(String::as_str), &widths);
    }
    output.push('\n');
}

fn render_table_separator(output: &mut String, widths: &[usize]) {
    output.push('|');
    for width in widths {
        let _ = write!(output, " {} |", "-".repeat((*width).max(3)));
    }
    output.push('\n');
}

fn render_table_row<'a>(
    output: &mut String,
    cells: impl Iterator<Item = &'a str>,
    widths: &[usize],
) {
    let cells = cells.collect::<Vec<_>>();
    output.push('|');
    for (index, width) in widths.iter().enumerate() {
        let cell = cells.get(index).copied().unwrap_or_default();
        let cell = truncate_cell(cell);
        let padding = width.saturating_sub(display_width(&cell));
        let _ = write!(output, " {cell}{} |", " ".repeat(padding));
    }
    output.push('\n');
}

fn truncate_cell(value: &str) -> String {
    let value = value.replace(['\n', '\r', '\t'], " ");
    if value.chars().count() <= MAX_CELL_CHARS {
        return value;
    }
    let mut truncated = value.chars().take(MAX_CELL_CHARS - 1).collect::<String>();
    truncated.push('…');
    truncated
}

fn display_width(value: &str) -> usize {
    value.chars().count()
}

fn display_scalar(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::String(value) => value.replace('|', "\\|"),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => display_inline_array(values),
        Value::Object(object) => {
            let fields = object
                .iter()
                .take(SAMPLE_ITEMS)
                .map(|(key, value)| format!("{key}: {}", display_scalar(value)))
                .collect::<Vec<_>>();
            let suffix = if object.len() > SAMPLE_ITEMS {
                ", …"
            } else {
                ""
            };
            format!("{{{}{suffix}}}", fields.join(", "))
        }
    }
}

fn display_inline_array(values: &[Value]) -> String {
    let items = values
        .iter()
        .take(SAMPLE_ITEMS)
        .map(display_scalar)
        .collect::<Vec<_>>();
    let suffix = if values.len() > SAMPLE_ITEMS {
        ", …"
    } else {
        ""
    };
    format!("[{}{suffix}]", items.join(", "))
}
