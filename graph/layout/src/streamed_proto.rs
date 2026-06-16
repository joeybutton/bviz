//! Reader for Bazel's `--output=streamed_proto`: a sequence of length-delimited
//! messages (a varint byte count followed by that many bytes), repeated to EOF.

use bazel_query_proto::analysis::{ActionGraphContainer, CqueryResult};
use bazel_query_proto::blaze_query::Target;
use prost::Message;

/// Parses every `ActionGraphContainer` from an aquery streamed_proto buffer.
pub fn read_action_graph_containers(
    mut buf: &[u8],
) -> Result<Vec<ActionGraphContainer>, prost::DecodeError> {
    let mut containers = Vec::new();
    while !buf.is_empty() {
        // Decoding advances `buf` past the consumed length prefix and message.
        containers.push(ActionGraphContainer::decode_length_delimited(&mut buf)?);
    }
    Ok(containers)
}

/// Parses every `Target` from a query streamed_proto buffer.
pub fn read_query_targets(mut buf: &[u8]) -> Result<Vec<Target>, prost::DecodeError> {
    let mut targets = Vec::new();
    while !buf.is_empty() {
        targets.push(Target::decode_length_delimited(&mut buf)?);
    }
    Ok(targets)
}

/// Parses every `CqueryResult` from a cquery streamed_proto buffer.
pub fn read_cquery_results(mut buf: &[u8]) -> Result<Vec<CqueryResult>, prost::DecodeError> {
    let mut results = Vec::new();
    while !buf.is_empty() {
        results.push(CqueryResult::decode_length_delimited(&mut buf)?);
    }
    Ok(results)
}
