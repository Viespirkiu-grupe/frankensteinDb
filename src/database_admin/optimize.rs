use std::time::Instant;

use super::*;

const MAX_EXPLICIT_MERGE_THREADS: usize = 64;

impl Database {
    /// Optimizes a table using the default retained segment and merge concurrency settings.
    pub fn optimize_table(&mut self, name: &str) -> Result<QueryResult> {
        let result = self.optimize_table_with_options(name, OptimizeOptions::default())?;
        Ok(QueryResult::message(format!(
            "optimized {} segment(s) to {} in {}",
            result.segments_before, result.segments_after, result.table
        )))
    }

    /// Merges balanced, disjoint segment groups concurrently until at most the target remains.
    ///
    /// This does not reindex documents. A table already below the target is left unchanged, and a
    /// single existing segment cannot be split into several segments by optimization.
    pub fn optimize_table_with_options(
        &mut self,
        name: &str,
        optimize: OptimizeOptions,
    ) -> Result<OptimizeResult> {
        ensure!(
            optimize.target_segments > 0,
            "target_segments must be positive"
        );
        ensure!(
            optimize.merge_threads <= MAX_EXPLICIT_MERGE_THREADS,
            "merge_threads must not exceed {MAX_EXPLICIT_MERGE_THREADS}"
        );
        let merge_threads = resolve_merge_threads(optimize.merge_threads);
        let started = Instant::now();
        self.flush()?;
        let def = self.table(name)?;
        let database_options = self.options.clone();
        let handle = self.index_handle_mut(&def)?;
        let old_writer = handle.writer.take().expect("initialized index writer");
        old_writer.wait_merging_threads()?;
        handle.writer = Some(new_index_writer_with_merge_threads(
            &handle.index,
            &database_options,
            merge_threads,
        )?);

        let segment_metas = handle.index.searchable_segment_metas()?;
        let segments_before = segment_metas.len();
        let groups = balanced_merge_groups(&segment_metas, optimize.target_segments);
        let merge_operations = groups.iter().filter(|group| group.len() > 1).count();
        if merge_operations > 0 {
            let writer = handle.writer.as_mut().expect("initialized index writer");
            let merge_futures = groups
                .iter()
                .filter(|group| group.len() > 1)
                .map(|group| writer.merge(group))
                .collect::<Vec<_>>();
            for merge in merge_futures {
                merge.wait()?;
            }
            handle.reader.reload()?;
        }
        let segments_after = handle.index.searchable_segment_ids()?.len();
        Ok(OptimizeResult {
            table: def.name,
            segments_before,
            segments_after,
            target_segments: optimize.target_segments,
            merge_threads,
            merge_operations,
            duration_ms: started.elapsed().as_secs_f64() * 1_000.0,
        })
    }
}

fn resolve_merge_threads(configured: usize) -> usize {
    if configured > 0 {
        return configured;
    }
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .clamp(1, 4)
}

fn balanced_merge_groups(
    segments: &[tantivy::SegmentMeta],
    target_segments: usize,
) -> Vec<Vec<tantivy::index::SegmentId>> {
    if segments.len() <= target_segments {
        return segments.iter().map(|segment| vec![segment.id()]).collect();
    }
    let group_count = target_segments.min(segments.len());
    let mut ordered = segments.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|segment| std::cmp::Reverse(segment.num_docs()));
    let mut groups = (0..group_count)
        .map(|_| (0_u64, Vec::new()))
        .collect::<Vec<_>>();
    for segment in ordered {
        let group = groups
            .iter_mut()
            .min_by_key(|(documents, ids)| (*documents, ids.len()))
            .expect("positive target creates at least one group");
        group.0 += u64::from(segment.num_docs());
        group.1.push(segment.id());
    }
    groups.into_iter().map(|(_, ids)| ids).collect()
}
