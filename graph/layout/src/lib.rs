//! Lays out a Bazel build graph as a compound graph using elk-rs.
//!
//! Reads `--output=streamed_proto` bytes from `aquery`, `cquery`, or `query`
//! into a common package-grouped compound graph and produces absolute-
//! coordinate box and connector geometry. aquery yields a target ->
//! configured-target -> action/artifact hierarchy; query and cquery yield one
//! node per target with edges from each rule's inputs.
pub mod aquery;
pub mod cquery;
pub mod graph;
mod label;
pub mod layout;
pub mod query;
pub mod quotient;
pub mod streamed_proto;

pub use graph::Graph;
pub use layout::{layout, Box, Connector, LaidOut};
