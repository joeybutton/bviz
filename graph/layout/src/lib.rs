//! Lays out a Bazel aquery action graph as a compound graph using elk-rs.
//!
//! Reads `aquery --output=streamed_proto` bytes into a target ->
//! configured-target -> action/artifact hierarchy and produces absolute-
//! coordinate box and connector geometry.
pub mod aquery;
pub mod graph;
pub mod layout;
pub mod quotient;
pub mod streamed_proto;

pub use graph::Graph;
pub use layout::{layout, Box, Connector, LaidOut};
