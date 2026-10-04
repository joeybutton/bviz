//! Generates Rust constants for the field numbers of a `.proto` file's messages,
//! read from a descriptor set that holds it: one module per message, named as
//! prost names the message's module, nesting as the messages nest.

use std::fs::File;
use std::io::{self, BufWriter, Write};

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost::Message;
use prost_types::{DescriptorProto, FileDescriptorSet};

const USAGE: &str = "usage: proto_field_numbers_gen <descriptor set> <proto file> <output.rs>";

const INDENT: &str = "    ";

fn main() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(descriptor_set), Some(proto_file), Some(output), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        panic!("{USAGE}");
    };
    let set = FileDescriptorSet::decode(std::fs::read(&descriptor_set)?.as_slice())
        .expect("decode the descriptor set");
    let file = set
        .file
        .iter()
        .find(|file| file.name() == proto_file)
        .unwrap_or_else(|| panic!("{proto_file} is not in {descriptor_set}"));

    let mut out = BufWriter::new(File::create(output)?);
    writeln!(out, "// @generated from {proto_file}.")?;
    writeln!(out, "#![allow(dead_code)]")?;
    for message in &file.message_type {
        write_message(&mut out, message, 0)?;
    }
    out.flush()
}

fn write_message(out: &mut impl Write, message: &DescriptorProto, depth: usize) -> io::Result<()> {
    let indent = INDENT.repeat(depth);
    writeln!(out)?;
    writeln!(out, "{indent}pub mod {} {{", message.name().to_snake_case())?;
    for field in &message.field {
        writeln!(
            out,
            "{indent}{INDENT}pub const {}: u32 = {};",
            field.name().to_shouty_snake_case(),
            field.number(),
        )?;
    }
    for nested in &message.nested_type {
        write_message(out, nested, depth + 1)?;
    }
    writeln!(out, "{indent}}}")
}
