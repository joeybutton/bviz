//! Reader for Bazel's `--output=streamed_proto`: a sequence of length-delimited
//! messages (a varint byte count followed by that many bytes), repeated to EOF.

use bazel_query_proto::analysis::ActionGraphContainer;
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
