//! Loads a Bazel `aquery --output=streamed_proto` file through
//! `bviz_graph_layout`, lays the action graph out, and opens a window
//! summarizing the result.

use bviz_graph_layout::{aquery, layout, streamed_proto, LaidOut};
use bviz_ui_shell::Chrome;
use gpui::{Context, FocusHandle, IntoElement, Render, Window, div, prelude::*, px};

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
        let body = div()
            .flex_1()
            .flex()
            .flex_col()
            .justify_center()
            .items_center()
            .gap_4()
            .p_4();
        let body = match &self.source {
            Source::Loaded(g) => {
                let containers = g.boxes.iter().filter(|b| b.compound).count();
                let leaves = g.boxes.len() - containers;
                let edges = g.connectors.len();
                body.child(
                    div()
                        .text_xl()
                        .text_color(colors.text)
                        .child("Bazel action graph"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(colors.disabled)
                        .child(format!(
                            "{containers} containers, {leaves} leaves, {edges} edges"
                        )),
                )
            }
            Source::Message(msg) => body.child(
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
