//! Generates the C header declaring the types gpui's Metal shaders share with Rust.
//!
//! Modified from source in the Zed editor; NOTICE names it.

use std::path::PathBuf;

/// Roots of the exported type graph; cbindgen emits these and everything they
/// contain.
const EXPORTED_TYPES: &[&str] = &[
    "Bounds",
    "Corners",
    "Edges",
    "Size",
    "Pixels",
    "PointF",
    "Hsla",
    "ContentMask",
    "AtlasTile",
    "PathRasterizationInputIndex",
    "PathVertex_ScaledPixels",
    "PathRasterizationVertex",
    "ShadowInputIndex",
    "Shadow",
    "QuadInputIndex",
    "Underline",
    "UnderlineInputIndex",
    "Quad",
    "BorderStyle",
    "SpriteInputIndex",
    "MonochromeSprite",
    "PolychromeSprite",
    "PathSprite",
    "SurfaceInputIndex",
    "SurfaceBounds",
    "TransformationMatrix",
];

fn main() {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let output_path = args
        .next()
        .expect("usage: scene_header_gen <output.h> <source.rs>...");
    let srcs: Vec<PathBuf> = args.collect();
    assert!(
        !srcs.is_empty(),
        "no Rust sources to generate bindings from"
    );

    let mut config = cbindgen::Config {
        include_guard: Some("SCENE_H".into()),
        language: cbindgen::Language::C,
        no_includes: true,
        ..Default::default()
    };
    config
        .export
        .include
        .extend(EXPORTED_TYPES.iter().map(|name| (*name).to_owned()));
    config.enumeration.prefix_with_name = true;

    let mut builder = cbindgen::Builder::new().with_config(config);
    for src in &srcs {
        builder = builder.with_src(src);
    }

    let bindings = builder.generate().expect("unable to generate bindings");

    let mut header = Vec::new();
    bindings.write(&mut header);
    let header = String::from_utf8(header).expect("cbindgen emits UTF-8");

    // cbindgen passes over a root naming no type without complaint, which would
    // leave the shaders referring to a type the header never declares.
    let unresolved: Vec<&&str> = EXPORTED_TYPES
        .iter()
        .filter(|name| !header.contains(**name))
        .collect();
    assert!(unresolved.is_empty(), "no gpui type named: {unresolved:?}");

    std::fs::write(&output_path, header).expect("write the generated header");
}
