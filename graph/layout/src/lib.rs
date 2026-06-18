//! Lays out a Bazel build graph as a compound graph using elk-rs.
//!
//! Reads `--output=streamed_proto` bytes from `aquery`, `cquery`, or `query`
//! into a common package-grouped compound graph and produces absolute-
//! coordinate box and connector geometry. aquery yields a target ->
//! configured-target -> action/artifact hierarchy and cquery a target ->
//! configured-target hierarchy; query yields one node per target. Edges run
//! from each dependency to the target that consumes it.
pub mod aquery;
pub mod cquery;
pub mod graph;
pub mod joint;
mod label;
pub mod layout;
pub mod lod;
pub mod query;
pub mod quotient;
pub mod streamed_proto;

pub use graph::{EdgeKind, Graph, Kind};
pub use joint::build_joint;
pub use layout::{layout, Box, Connector, LaidOut};
