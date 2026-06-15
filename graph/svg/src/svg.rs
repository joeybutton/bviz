//! Renders laid-out geometry as a standalone SVG document.

use std::fmt::Write;

use bviz_graph_layout::LaidOut;

const MARGIN: f64 = 16.0;

pub fn emit(layout: &LaidOut) -> String {
    let width = layout.width + 2.0 * MARGIN;
    let height = layout.height + 2.0 * MARGIN;

    let mut svg = String::new();
    svg.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = write!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w}\" height=\"{h}\" \
         viewBox=\"0 0 {w} {h}\" font-family=\"sans-serif\">\n",
        w = num(width),
        h = num(height),
    );
    svg.push_str(
        "<defs><marker id=\"arrow\" viewBox=\"0 0 10 10\" refX=\"9\" refY=\"5\" \
         markerWidth=\"8\" markerHeight=\"8\" orient=\"auto-start-reverse\">\
         <path d=\"M0,0 L10,5 L0,10 z\" fill=\"#5a5a5a\"/></marker></defs>\n",
    );
    let _ = write!(
        svg,
        "<g transform=\"translate({m},{m})\">\n",
        m = num(MARGIN)
    );

    // Boxes arrive parent-before-child, so children draw over their containers.
    for b in &layout.boxes {
        if b.compound {
            let _ = write!(
                svg,
                "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"6\" \
                 fill=\"{fill}\" stroke=\"#3a5a8c\" stroke-width=\"1.5\"/>\n",
                x = num(b.x),
                y = num(b.y),
                w = num(b.width),
                h = num(b.height),
                fill = container_fill(&b.kind),
            );
            let _ = write!(
                svg,
                "<text x=\"{x}\" y=\"{y}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
                 font-size=\"13\" font-weight=\"bold\" fill=\"#1f3354\">{label}</text>\n",
                x = num(b.x + b.width / 2.0),
                y = num(b.y + 12.0),
                label = escape(&b.label),
            );
        } else {
            let _ = write!(
                svg,
                "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" rx=\"6\" \
                 fill=\"{fill}\" stroke=\"#3a5a8c\" stroke-width=\"1.5\"/>\n",
                x = num(b.x),
                y = num(b.y),
                w = num(b.width),
                h = num(b.height),
                fill = leaf_fill(&b.kind),
            );
            let _ = write!(
                svg,
                "<text x=\"{x}\" y=\"{y}\" text-anchor=\"middle\" dominant-baseline=\"central\" \
                 font-size=\"13\" fill=\"#1f3354\">{label}</text>\n",
                x = num(b.x + b.width / 2.0),
                y = num(b.y + b.height / 2.0),
                label = escape(&b.label),
            );
        }
    }

    // Connectors last: an edge into a nested node crosses the opaque fills of
    // the boxes it descends through, so it must paint over them to stay visible.
    for connector in &layout.connectors {
        svg.push_str("<polyline points=\"");
        for (i, (x, y)) in connector.points.iter().enumerate() {
            if i > 0 {
                svg.push(' ');
            }
            let _ = write!(svg, "{},{}", num(*x), num(*y));
        }
        svg.push_str(
            "\" fill=\"none\" stroke=\"#5a5a5a\" stroke-width=\"1.5\" marker-end=\"url(#arrow)\"/>\n",
        );
    }

    svg.push_str("</g></svg>\n");
    svg
}

// Container fills nest visually: the outer package box is a deeper slate so the
// lighter target and configured-target boxes read as sitting inside it. An
// expanded group of files gets a green-tinted box, coherent with the file leaf
// fills it holds and distinct from the blue package/target containers.
fn container_fill(kind: &str) -> &'static str {
    match kind {
        "package" => "#c4d2e8",
        "group" => "#cfe8d8",
        _ => "#dde6f2",
    }
}

// Distinct fills make actions, their outputs, and sources easy to tell apart.
fn leaf_fill(kind: &str) -> &'static str {
    match kind {
        "action" => "#fde8c8",
        "artifact" => "#d6efd6",
        "source" => "#e8e0f4",
        _ => "#eef3fb",
    }
}

fn num(value: f64) -> String {
    if value == value.round() && value.is_finite() {
        format!("{}", value as i64)
    } else {
        format!("{value:.3}")
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
