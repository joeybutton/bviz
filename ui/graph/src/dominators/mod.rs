//! What exists in a build only because of each of its actions, configured
//! targets or targets: the dominator tree of the graph's points of one unit,
//! drawn as an icicle.

mod icicle;
mod paint;

use crate::Palette;
use crate::model::{DominatorsLanded, GraphModel, SourceReplaced};
use crate::point::unit_names;
use crate::toolbar::{busy, group};
use crate::unit_picker::UnitPicker;
use bviz_graph_layout::dominators::Dominators;
use bviz_graph_layout::metrics::Scope;
use bviz_ui_components::{Toolbar, cached_layer, centered_notice, centered_spinner};
use bviz_ui_workspace::{Panel, PanelEvent, PanelKind, ViewKind};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Pixels,
    Render, SharedString, Subscription, WeakEntity, Window, canvas, div, prelude::*, px,
};
use paint::Scene;
use std::sync::Arc;

/// The key context scoping the dominators panel's own bindings, so a key
/// pressed here reaches this panel and no other.
pub(crate) const KEY_CONTEXT: &str = "GraphDominators";

/// What the panel reads while the tree is being computed.
const COMPUTING: &str = "Computing dominators…";

/// The dominator tree of a graph's points, drawn as an icicle.
pub struct DominatorsPanel {
    focus_handle: FocusHandle,
    /// The graph whose tree is drawn, which computes it.
    model: Entity<GraphModel>,
    /// Whether the workspace is showing the panel, which a graph landing while
    /// it is shown has its tree computed for at once.
    shown: bool,
    /// Chooses the unit drawn, from the time the graph is read.
    picker: Option<UnitPicker>,
    /// The tree drawn, from the time the first one lands.
    drawn: Option<Drawn>,
    /// The layer drawing the cells; notified through
    /// [`Self::invalidate_canvas`].
    canvas: Entity<IcicleCanvas>,
    /// Redraws as the graph is read.
    _changed: Subscription,
    _source_replaced: Subscription,
    _dominators_landed: Subscription,
}

impl DominatorsPanel {
    pub(crate) fn new(model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let changed = cx.observe(&model, |_, _, cx| cx.notify());
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            view.drawn = None;
            view.picker = unit_picker(&view.model, cx);
            view.compute(cx);
        });
        let dominators_landed = cx.subscribe(&model, |view, _, landed: &DominatorsLanded, cx| {
            if view.scope(cx) == Some(landed.scope) {
                view.redraw(cx);
            }
        });
        let parent = cx.entity().downgrade();
        Self {
            focus_handle: cx.focus_handle(),
            picker: unit_picker(&model, cx),
            model,
            shown: false,
            drawn: None,
            canvas: cx.new(|_| IcicleCanvas { parent }),
            _changed: changed,
            _source_replaced: source_replaced,
            _dominators_landed: dominators_landed,
        }
    }

    /// The scope whose tree is drawn, once the graph is read.
    fn scope(&self, cx: &App) -> Option<Scope> {
        self.picker.as_ref().map(|picker| picker.chosen(cx))
    }

    /// Has the tree of the chosen scope computed, where the panel is shown, and
    /// draws it where it already is.
    fn compute(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx).filter(|_| self.shown) else {
            return;
        };
        self.model
            .update(cx, |model, cx| model.want_dominators(&[scope], cx));
        self.redraw(cx);
    }

    /// Draws the unit chosen.
    fn choose(&mut self, cx: &mut Context<Self>) {
        self.compute(cx);
        cx.notify();
    }

    /// Draws the tree of the chosen scope where it has landed and is not the
    /// one drawn. The tree drawn stays until then.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = self.scope(cx) else {
            return;
        };
        let Some(tree) = self.model.read(cx).dominators(scope).cloned() else {
            return;
        };
        if self
            .drawn
            .as_ref()
            .is_some_and(|drawn| Arc::ptr_eq(&drawn.tree, &tree))
        {
            return;
        }
        self.drawn = Some(Drawn { tree });
        self.invalidate_canvas(cx);
    }

    /// Repaints the cells along with the layer over them; every change to what
    /// the cells show routes through here.
    fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }
}

/// The picker among the units the graph `model` has read, the finest chosen;
/// absent until the graph is read.
fn unit_picker(
    model: &Entity<GraphModel>,
    cx: &mut Context<DominatorsPanel>,
) -> Option<UnitPicker> {
    let inputs = model.read(cx).loaded()?.shared.inputs;
    UnitPicker::new("dominators-unit", inputs, DominatorsPanel::choose, cx)
}

/// The tree drawn.
struct Drawn {
    tree: Arc<Dominators>,
}

impl EventEmitter<PanelEvent> for DominatorsPanel {}

impl Focusable for DominatorsPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for DominatorsPanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("graph-dominators"),
            label: "Dominators",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        Self::kind().label.into()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        let model = self.model.read(cx);
        let status = model.source().status("No graph", |_| match &self.drawn {
            None => COMPUTING.into(),
            Some(drawn) => {
                let many = unit_names(drawn.tree.scope().unit()).many;
                format!("{} {many}", drawn.tree.total())
            }
        });
        Some(status.into())
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.shown = active;
        if active {
            self.focus_handle.focus(window, cx);
            self.compute(cx);
        }
    }
}

impl Render for DominatorsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let root = div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .flex_1()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden();
        let model = self.model.read(cx);
        if let Some(placeholder) = model.source().placeholder(&palette.colors) {
            return root.child(placeholder);
        }
        let drawn_scope = self.drawn.as_ref().map(|drawn| drawn.tree.scope());
        let root = root.children(self.picker.as_ref().map(|picker| {
            let computing = drawn_scope.is_some_and(|drawn| drawn != picker.chosen(cx));
            strip(picker, computing, &palette)
        }));
        let Some(drawn) = &self.drawn else {
            return root.child(centered_spinner(COMPUTING, &palette.colors));
        };
        if drawn.tree.total() == 0 {
            let many = unit_names(drawn.tree.scope().unit()).many;
            let empty = format!("The graph holds no {many}.");
            return root.child(centered_notice(empty, &palette.colors));
        }
        root.child(
            div()
                .id("dominators-icicle")
                .relative()
                .flex_1()
                .min_h(px(0.))
                .child(cached_layer(self.canvas.clone())),
        )
    }
}

/// The strip choosing the unit drawn, which reads `computing` while the tree of
/// a new choice is computed over the one drawn for the last.
fn strip(picker: &UnitPicker, computing: bool, palette: &Palette) -> Toolbar {
    Toolbar::new()
        .child(group("points", palette).child(picker.dropdown()))
        .when(computing, |bar| {
            bar.trailing(busy("dominators-computing", "computing…", palette))
        })
}

/// The layer the cells are drawn in, cached across frames that leave the tree
/// drawn unchanged.
struct IcicleCanvas {
    /// The panel whose tree this draws.
    parent: WeakEntity<DominatorsPanel>,
}

impl Render for IcicleCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let scene = self.parent.upgrade().and_then(|parent| {
            let panel = parent.read(cx);
            let tree = Arc::clone(&panel.drawn.as_ref()?.tree);
            let shared = panel.model.read(cx).loaded()?.shared.clone();
            Some(Scene {
                tree,
                shared,
                palette,
            })
        });
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, cx| {
                if let Some(scene) = scene {
                    paint::paint_icicle(&scene, bounds, window, cx);
                }
            },
        )
        .size_full()
    }
}
