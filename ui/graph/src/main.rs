//! Loads a Bazel `aquery --output=streamed_proto` file through
//! `bviz_graph_layout`, lays the action graph out, and opens a window
//! summarizing the result.

use bviz_graph_layout::{aquery, layout, streamed_proto, LaidOut};
use bviz_ui_shell::Chrome;
use gpui::{
    Context, FontWeight, FocusHandle, IntoElement, Render, Window, div, prelude::*, px, rgb,
};

/// Padding around the graph, matching the SVG renderer's margin.
const MARGIN: f64 = 16.0;

/// Fill color for a leaf box, keyed by its node kind, matching the SVG palette.
fn leaf_fill(kind: &str) -> u32 {
    match kind {
        "action" => 0xfde8c8,
        "artifact" => 0xd6efd6,
        "source" => 0xe8e0f4,
        _ => 0xeef3fb,
    }
}

/// The loaded graph, or a message explaining why none is shown.
enum Source {
    Loaded(LaidOut),
    Message(String),
}

/// Loads, lays out, and summarizes an aquery action graph.
struct GraphView {
    focus_handle: FocusHandle,
    source: Source,
}

impl GraphView {
    fn new(source: Source, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            focus_handle,
            source,
        }
    }

    /// Builds the absolutely-positioned box elements at their layout
    /// coordinates: containers carry a top label band, leaves are filled by
    /// kind. Boxes are emitted parent-before-child so children paint over the
    /// containers that enclose them.
    fn graph(g: &LaidOut) -> impl IntoElement {
        let mut container = div()
            .relative()
            .w(px((g.width + 2.0 * MARGIN) as f32))
            .h(px((g.height + 2.0 * MARGIN) as f32));
        for b in &g.boxes {
            let frame = div()
                .absolute()
                .left(px((b.x + MARGIN) as f32))
                .top(px((b.y + MARGIN) as f32))
                .w(px(b.width as f32))
                .h(px(b.height as f32))
                .rounded(px(6.))
                .border_1()
                .border_color(rgb(0x3a5a8c));
            let frame = if b.compound {
                frame.bg(rgb(0xdde6f2)).flex().flex_col().items_center().child(
                    div()
                        .h(px(26.))
                        .flex()
                        .items_center()
                        .text_size(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(0x1f3354))
                        .child(b.label.clone()),
                )
            } else {
                frame
                    .bg(rgb(leaf_fill(&b.kind)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.))
                    .text_color(rgb(0x1f3354))
                    .child(b.label.clone())
            };
            container = container.child(frame);
        }
        container
    }

    fn status(&self) -> String {
        match &self.source {
            Source::Loaded(g) => format!(
                "{} boxes · {} connectors · {}×{}",
                g.boxes.len(),
                g.connectors.len(),
                g.width.round() as i64,
                g.height.round() as i64,
            ),
            Source::Message(_) => "No graph".into(),
        }
    }
}

impl Render for GraphView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let body = match &self.source {
            Source::Loaded(g) => div()
                .flex_1()
                .flex()
                .justify_center()
                .items_center()
                .overflow_hidden()
                .child(Self::graph(g)),
            Source::Message(msg) => div()
                .flex_1()
                .flex()
                .flex_col()
                .justify_center()
                .items_center()
                .gap_4()
                .p_4()
                .child(
                    div()
                        .max_w(px(400.))
                        .text_sm()
                        .text_color(colors.disabled)
                        .child(msg.clone()),
                ),
        };
        Chrome::new(&self.focus_handle)
            .title("bazel_viz graph")
            .status(self.status())
            .child(body)
    }
}

/// Reads the aquery proto at `path` and lays out its action graph.
fn load(path: &str) -> Result<LaidOut, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let containers =
        streamed_proto::read_action_graph_containers(&bytes).map_err(|e| e.to_string())?;
    let graph = aquery::build(&containers, false);
    Ok(layout::layout(&graph))
}

fn main() {
    let source = match std::env::args().nth(1) {
        Some(path) => match load(&path) {
            Ok(laid_out) => Source::Loaded(laid_out),
            Err(err) => Source::Message(err),
        },
        None => Source::Message("Usage: graph <aquery.streamed_proto>".into()),
    };
    bviz_ui_shell::run("bazel_viz graph", move |window, cx| {
        cx.new(|cx| GraphView::new(source, window, cx))
    });
}
