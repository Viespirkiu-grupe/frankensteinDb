mod compiler;
mod executor;
mod filter;
mod values;

pub(crate) use executor::{
    collect_aggregation_results, collect_standard_aggregations, standard_aggregation_worker_count,
};
pub(super) use filter::typed_filter_aggregation;

use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tantivy::aggregation::DistributedAggregationCollector;
use tantivy::aggregation::intermediate_agg_result::IntermediateAggregationResults;

use super::*;

pub(crate) use compiler::compile_aggregations;

const MAX_INTERMEDIATE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct IntermediateEnvelope {
    version: u8,
    request_hash: [u8; 32],
    result: IntermediateAggregationResults,
}

pub(crate) fn collect_intermediate(
    searcher: &Searcher,
    query: &dyn Query,
    request: &Aggregations,
    index: &Index,
) -> Result<Vec<u8>> {
    let collector =
        DistributedAggregationCollector::from_aggs(request.clone(), aggregation_context(index));
    let result = searcher.search(query, &collector)?;
    Ok(binary_options().serialize(&IntermediateEnvelope {
        version: 1,
        request_hash: aggregation_hash(request)?,
        result,
    })?)
}

pub(crate) fn merge_intermediates(request: Aggregations, payloads: &[Vec<u8>]) -> Result<Value> {
    ensure!(
        !payloads.is_empty(),
        "at least one intermediate payload is required"
    );
    ensure!(
        payloads.len() <= 1_024,
        "at most 1024 intermediate payloads are allowed"
    );
    let expected_hash = aggregation_hash(&request)?;
    let mut merged = IntermediateAggregationResults::default();
    let mut total_bytes = 0u64;
    for payload in payloads {
        total_bytes = total_bytes
            .checked_add(payload.len() as u64)
            .context("intermediate payload size overflow")?;
        ensure!(
            total_bytes <= MAX_INTERMEDIATE_BYTES,
            "intermediate payloads exceed 256 MiB"
        );
        let envelope: IntermediateEnvelope = binary_options().deserialize(payload)?;
        ensure!(
            envelope.version == 1,
            "unsupported intermediate payload version"
        );
        ensure!(
            envelope.request_hash == expected_hash,
            "intermediate payload aggregation request does not match"
        );
        merged.merge_fruits(envelope.result)?;
    }
    let result = merged.into_final_result(
        request,
        AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(10_000)),
    )?;
    Ok(serde_json::to_value(result)?)
}

pub(crate) fn aggregation_context(index: &Index) -> AggContextParams {
    aggregation_context_with_limits(
        index,
        AggregationLimitsGuard::new(Some(128 * 1024 * 1024), Some(10_000)),
    )
}

pub(crate) fn aggregation_context_with_limits(
    index: &Index,
    limits: AggregationLimitsGuard,
) -> AggContextParams {
    AggContextParams::new(limits, index.tokenizers().clone())
}

fn aggregation_hash(request: &Aggregations) -> Result<[u8; 32]> {
    // `Aggregations` uses hash maps internally; `Value::Object` canonicalizes map order
    // with serde_json's default sorted map before the cross-process identity hash is computed.
    let canonical = serde_json::to_value(request)?;
    Ok(Sha256::digest(serde_json::to_vec(&canonical)?).into())
}

fn binary_options() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_INTERMEDIATE_BYTES)
        .reject_trailing_bytes()
}
