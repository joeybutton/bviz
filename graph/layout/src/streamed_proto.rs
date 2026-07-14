//! Reader for Bazel's `--output=streamed_proto`: a sequence of length-delimited
//! messages (a varint byte count followed by that many bytes), repeated to EOF.

use bazel_query_proto::analysis::{ActionGraphContainer, CqueryResult};
use bazel_query_proto::blaze_query::Target;
use prost::Message;

/// Decodes every length-delimited `M` from a streamed_proto buffer, in order.
fn read_length_delimited<M: Message + Default>(
    mut buf: &[u8],
) -> Result<Vec<M>, prost::DecodeError> {
    let mut messages = Vec::new();
    while !buf.is_empty() {
        // Decoding advances `buf` past the consumed length prefix and message.
        messages.push(M::decode_length_delimited(&mut buf)?);
    }
    Ok(messages)
}

/// Parses every `ActionGraphContainer` from an aquery streamed_proto buffer.
pub fn read_action_graph_containers(
    buf: &[u8],
) -> Result<Vec<ActionGraphContainer>, prost::DecodeError> {
    read_length_delimited(buf)
}

/// Parses every `Target` from a query streamed_proto buffer.
pub fn read_query_targets(buf: &[u8]) -> Result<Vec<Target>, prost::DecodeError> {
    read_length_delimited(buf)
}

/// Parses every `CqueryResult` from a cquery streamed_proto buffer.
pub fn read_cquery_results(buf: &[u8]) -> Result<Vec<CqueryResult>, prost::DecodeError> {
    read_length_delimited(buf)
}
