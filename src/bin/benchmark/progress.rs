use super::*;

pub(crate) struct ProgressReporter {
    enabled: bool,
    terminal: bool,
    last_ingestion_draw: std::cell::Cell<Instant>,
    next_non_terminal_percent: std::cell::Cell<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct IngestionTimings {
    pub(crate) input_wait: Duration,
    pub(crate) staging: Duration,
    pub(crate) flushing: Duration,
}

impl ProgressReporter {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            terminal: std::io::stderr().is_terminal(),
            last_ingestion_draw: std::cell::Cell::new(Instant::now()),
            next_non_terminal_percent: std::cell::Cell::new(10),
        }
    }

    pub(crate) fn message(&self, message: impl std::fmt::Display) {
        if self.enabled {
            eprintln!("{message}");
        }
    }

    pub(crate) fn ingestion(
        &self,
        bytes: u64,
        total: u64,
        rows: usize,
        elapsed: Duration,
        timings: &IngestionTimings,
    ) {
        if !self.enabled {
            return;
        }
        let percent = bytes.saturating_mul(100) / total.max(1);
        if self.terminal {
            if bytes < total
                && self.last_ingestion_draw.get().elapsed() < Duration::from_millis(200)
            {
                return;
            }
            self.last_ingestion_draw.set(Instant::now());
            let rows_per_second = rows as f64 / elapsed.as_secs_f64().max(f64::EPSILON);
            let elapsed_seconds = elapsed.as_secs_f64().max(f64::EPSILON);
            eprint!(
                "\r  ingest {:>3}% | {:>7} rows | {:>8.0} rows/s | {:>6.1}s | wait {:>3.0}% stage {:>3.0}% flush {:>3.0}%",
                percent.min(100),
                rows,
                rows_per_second,
                elapsed.as_secs_f64(),
                timings.input_wait.as_secs_f64() * 100.0 / elapsed_seconds,
                timings.staging.as_secs_f64() * 100.0 / elapsed_seconds,
                timings.flushing.as_secs_f64() * 100.0 / elapsed_seconds,
            );
            let _ = std::io::stderr().flush();
        } else if percent >= self.next_non_terminal_percent.get() {
            eprintln!("  ingest {:>3}% | {:>7} rows", percent.min(100), rows);
            self.next_non_terminal_percent
                .set((percent / 10 + 1).saturating_mul(10));
        }
    }

    pub(crate) fn ingestion_commit_started(
        &self,
        total: u64,
        rows: usize,
        elapsed: Duration,
        timings: &IngestionTimings,
    ) {
        self.ingestion(total, total, rows, elapsed, timings);
        if self.enabled && self.terminal {
            eprintln!();
        }
        self.message("committing Tantivy index");
    }

    pub(crate) fn ingestion_finished(
        &self,
        rows: usize,
        elapsed: Duration,
        timings: &IngestionTimings,
    ) {
        self.message(format!(
            "ingestion complete: {rows} rows in {:.1}s ({:.0} rows/s)",
            elapsed.as_secs_f64(),
            rows as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
        ));
        self.message(format!(
            "  phases: input wait {:.1}s, SQLite + Tantivy staging {:.1}s, flush {:.1}s",
            timings.input_wait.as_secs_f64(),
            timings.staging.as_secs_f64(),
            timings.flushing.as_secs_f64(),
        ));
    }

    pub(crate) fn benchmark_start(
        &self,
        current: usize,
        total: usize,
        name: &str,
        iterations: usize,
    ) {
        self.message(format!(
            "benchmark {current}/{total}: {name} ({iterations} measured iteration(s))"
        ));
    }

    pub(crate) fn benchmark_done(&self, measurement: &Measurement) {
        self.message(format!("  done: median {:.3} ms", measurement.median_ms));
    }
}
