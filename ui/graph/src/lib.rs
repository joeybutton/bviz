//! Loads a Bazel `aquery`, `cquery`, or `query` `--output=streamed_proto` file
//! through `bviz_graph_layout`, lays the graph out, and opens a window
//! rendering the result.

use bviz_graph_layout::select::labels::LabelIndex;
use bviz_graph_layout::{Id, Kind};
use bviz_invocation::Session;
use bviz_ui_components::{
    DragEnd, Dropdown, DropdownEvent, MultiSelect, MultiSelectEvent, Pending, ScrollbarColors,
    cached_layer, overlay_canvas, text_input,
};
use bviz_ui_graph_args::{GraphArgs, GraphSource};
use bviz_ui_playback::{Clock, DocumentClock};
use bviz_ui_theme::{DataColors, Theme, ThemeColors};
use bviz_ui_workspace::{
    Member, Panel, PanelEvent, PanelGroup, PanelKind, ViewKind, ViewSpec, Weight, document_label,
};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render,
    Rgba, ScrollDelta, ScrollWheelEvent, SharedString, Subscription, WeakEntity, Window, actions,
    canvas, div, prelude::*, px, size,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod actions;
mod build;
mod camera;
mod checklist;
mod colour;
mod column;
mod critical;
mod derive;
mod drag;
mod duration;
mod emphasis;
mod exclusive;
mod framing;
mod hints;
mod history;
mod hue;
mod jump;
mod legend;
mod missing;
mod mode;
mod model;
mod morph;
mod neighborhood;
mod paint;
mod pins;
mod playback;
mod point;
mod preset;
mod relation;
mod report;
mod reveal;
mod rules;
mod scatter;
mod scroll;
mod search;
mod selection;
mod status;
mod timecard;
mod timing;
mod toolbar;
mod unit_picker;
mod why;

use camera::{Camera, Rect, from_screen, recenter_rect, zoom_step};
use checklist::OriginRow;
use colour::Colouring;
use derive::{Derived, Drawn, Expandable, LayoutLanded};
use drag::{Drag, end_drag, start_pan, track_gesture};
use emphasis::Emphasis;
pub use exclusive::ExclusivePanel;
use framing::{Framing, Target, build_prepaint};
use legend::Legend;
use mode::LayoutMode;
use model::{GraphModel, MapPlaced, Reveal, RevealRequested, SourceReplaced};
use morph::{Frame, Morph};
use paint::{Scene, TitleTarget, canvas_body};
use relation::Relation;
pub use scatter::ScatterPanel;
use scroll::{scrollbar_hit, start_scroll_drag};
use search::Search;
use selection::Selection;

actions!(
    bviz_graph,
    [
        ToggleNeighborhood,
        ToggleReduce,
        Dismiss,
        JumpBack,
        JumpForward
    ]
);

/// The key context scoping the graph's own bindings, so a key pressed here
/// reaches this panel and no other.
const KEY_CONTEXT: &str = "Graph";

pub fn register_keys(cx: &mut App) {
    cx.bind_keys(bindings());
}

fn bindings() -> Vec<KeyBinding> {
    // A bare character typed into a text field is text, so the panel's own
    // bindings stand down inside one.
    let outside_text = format!("{KEY_CONTEXT} && !{}", text_input::KEY_CONTEXT);
    let outside_text = Some(outside_text.as_str());
    let graph = Some(KEY_CONTEXT);
    let jumping = Some(jump::KEY_CONTEXT);
    let picking = format!("{} && {}", scatter::KEY_CONTEXT, scatter::PICKING);
    let picking = Some(picking.as_str());
    let exclusive = format!(
        "{} && !{}",
        exclusive::KEY_CONTEXT,
        exclusive::ROOTS_KEY_CONTEXT
    );
    let outside_roots = Some(exclusive.as_str());
    vec![
        KeyBinding::new("h", ToggleNeighborhood, outside_text),
        KeyBinding::new("r", ToggleReduce, outside_text),
        KeyBinding::new("escape", Dismiss, outside_text),
        KeyBinding::new("alt-left", JumpBack, outside_text),
        KeyBinding::new("alt-right", JumpForward, outside_text),
        KeyBinding::new("cmd-f", jump::Open, graph),
        KeyBinding::new("ctrl-f", jump::Open, graph),
        KeyBinding::new("/", jump::Open, outside_text),
        KeyBinding::new("up", jump::Previous, jumping),
        KeyBinding::new("down", jump::Next, jumping),
        KeyBinding::new("enter", jump::Go, jumping),
        KeyBinding::new("ctrl-enter", jump::AskWhy, jumping),
        KeyBinding::new("escape", jump::Close, jumping),
        KeyBinding::new("tab", jump::NextScope, jumping),
        KeyBinding::new("shift-tab", jump::PreviousScope, jumping),
        // Escape acts on the innermost thing holding focus: in the field that
        // is the card the typed pattern put up over the drawing.
        KeyBinding::new("escape", search::DismissResults, Some(search::KEY_CONTEXT)),
        KeyBinding::new("0", scatter::ShowWholePlot, Some(scatter::KEY_CONTEXT)),
        KeyBinding::new("0", exclusive::ShowWholeBuild, outside_roots),
        KeyBinding::new("backspace", exclusive::FocusUp, outside_roots),
        KeyBinding::new("left", exclusive::SelectPrevious, outside_roots),
        KeyBinding::new("right", exclusive::SelectNext, outside_roots),
        KeyBinding::new("up", exclusive::SelectParent, outside_roots),
        KeyBinding::new("down", exclusive::SelectChild, outside_roots),
        KeyBinding::new("enter", exclusive::FocusSelected, outside_roots),
        KeyBinding::new("g", exclusive::ShowSelected, outside_roots),
        KeyBinding::new("alt-left", exclusive::FocusBack, outside_roots),
        KeyBinding::new("alt-right", exclusive::FocusForward, outside_roots),
        KeyBinding::new(
            "escape",
            exclusive::LeaveRoots,
            Some(exclusive::ROOTS_KEY_CONTEXT),
        ),
        KeyBinding::new("up", scatter::AimUp, picking),
        KeyBinding::new("down", scatter::AimDown, picking),
        KeyBinding::new("enter", scatter::RevealAimed, picking),
        KeyBinding::new("escape", scatter::ClosePickList, picking),
    ]
}

/// Padding around the graph, in layout pixels.
const MARGIN: f64 = 16.0;

/// The share of the canvas a card floating over it may span. The cards sit at
/// opposite corners, so a share leaving a gap between two of them keeps either
/// from covering the other however narrow the canvas.
const CARD_MAX_SHARE: f32 = 0.45;

/// Largest zoom-in factor. The smallest is computed per graph so the whole graph
/// always fits the viewport.
const MAX_ZOOM: f32 = 4.0;

/// The most a frame request zooms in to show what it names, at which a label
/// reads at a comfortable size however small the box framed.
const MAX_FRAME_ZOOM: f32 = 1.5;

/// Factor one wheel notch scales the zoom by, small enough that a notch nudges
/// the view rather than jumping it.
const ZOOM_PER_NOTCH: f32 = 1.1;

/// Scroll delta one wheel notch amounts to, for the devices that report a delta
/// in pixels rather than in lines.
const PIXELS_PER_NOTCH: f32 = 120.0;

/// Fraction of the viewport the fitted graph spans, leaving a margin so the whole
/// graph sits within the inner part of the window rather than edge to edge.
const FIT_MARGIN: f32 = 2.0 / 3.0;

/// Breathing room held between the reframed content and the viewport edge when a
/// relayout settles the view, so a kept box never sits flush against the border.
const RECENTER_PAD: f32 = 24.0;

/// How long a toggle takes to come to rest — the camera easing onto the reframed
/// content, and each box onto its place in the new drawing, over the one stretch
/// of time. Short enough to stay responsive, long enough that the eye can follow
/// the shift instead of the view jumping there in a single frame.
const SETTLE_DURATION: Duration = Duration::from_millis(220);

/// Below these per-axis deltas a settle is a no-op and is not animated, so a
/// toggle that leaves the view already framed does not twitch.
const SETTLE_MIN_ZOOM_STEP: f32 = 0.002;
const SETTLE_MIN_PAN_STEP: f32 = 0.5;

/// Theme-resolved colors the canvas paints with, owned so the `'static` paint
/// closure can capture them.
#[derive(Clone, Copy)]
struct Palette {
    border: Rgba,
    /// Text on the controls framing the canvas.
    label: Rgba,
    /// Fill of a container box, and the sheet the legend floats on.
    container: Rgba,
    /// Fill of the outermost package box, which stands one step further off the
    /// canvas than `container` so nesting reads outermost first.
    package: Rgba,
    background: Rgba,
    /// Accent every emphasis is drawn in: the border picking out the selected
    /// box and the just-toggled one, and the wash over a marked box's fill.
    highlight: Rgba,
    /// The slots the node kinds and the edge provenances are colored from.
    data: DataColors,
    scroll: ScrollbarColors,
    /// The theme's own roles, for handing to a shared component that resolves
    /// its colors from them; the canvas paints from this palette's roles rather
    /// than from these.
    colors: ThemeColors,
}

impl Palette {
    fn from_theme(theme: &Theme) -> Palette {
        let colors = &theme.colors;
        Palette {
            border: colors.border.into(),
            label: colors.text.into(),
            container: colors.panel.into(),
            package: colors.surface.into(),
            background: colors.background.into(),
            highlight: colors.highlight.into(),
            data: theme.data,
            scroll: ScrollbarColors::from_colors(colors),
            colors: *colors,
        }
    }
}

/// A view of a Bazel build graph, drawn under a layout mode and camera of its
/// own.
pub struct GraphPanel {
    /// Names the document shown, labelling this panel's tab.
    title: SharedString,
    focus_handle: FocusHandle,
    /// The graph drawn here, shared with every other view of it.
    model: Entity<GraphModel>,
    /// The part of that graph this panel draws, and the state it draws it at.
    derived: Derived,
    framing: Framing,
    drag: Drag,
    emphasis: Emphasis,
    morph: Morph,
    mode: LayoutMode,
    mode_picker: Entity<Dropdown>,
    /// Carries the picker's selections into [`GraphPanel::set_mode`].
    _mode_choices: Subscription,
    colouring: Colouring,
    colour_picker: Entity<Dropdown>,
    /// Carries the picker's selections into [`colour::set`].
    _colour_choices: Subscription,
    /// The node classes the loaded graph holds boxes for, in the order the class
    /// checklist's rows stand.
    classes: Vec<Kind>,
    class_filter: Entity<MultiSelect>,
    /// Carries a toggled class row into the panel's filter.
    _class_choices: Subscription,
    /// The repository origins and repositories the loaded graph names, in the
    /// order the repository checklist's rows stand.
    origins: Vec<OriginRow>,
    origin_filter: Entity<MultiSelect>,
    /// Carries a toggled repository row into the panel's filter.
    _origin_choices: Subscription,
    /// The reductions the loaded inputs can answer, in the order the preset
    /// checklist's rows stand.
    presets: Vec<preset::Offer>,
    preset_filter: Entity<MultiSelect>,
    /// Carries a toggled preset row into the panel's filter.
    _preset_choices: Subscription,
    /// The label search, whose pattern shows or hides the nodes it names.
    search: Search,
    jump: jump::Finder,
    /// Carries what is typed into the search field into the search.
    _search_typing: Subscription,
    /// Redraws as the graph is read.
    _changed: Subscription,
    /// Repaints the drawing, and only it, as a gesture moves the document's
    /// clock while the boxes are coloured by playback.
    _clock_moved: Option<Subscription>,
    /// Derives what the selected mode draws as the read lands: a derivation
    /// asked for while the graph was still being read could not start.
    _source_replaced: Subscription,
    /// Derives the levels of the panel's restriction as the map lands.
    _map_placed: Subscription,
    _reveal_requested: Subscription,
    /// Box under the cursor.
    hovered: Option<Id>,
    /// The box whose related nodes stay marked, and how far that marking reaches;
    /// set by a secondary click and offered by the toolbar's relation picker.
    selection: Selection,
    /// Outlives the stretches where the strip leaves it out, so the option it
    /// stands on is still the one chosen when the next selection brings it back.
    relation_picker: Entity<Dropdown>,
    /// Carries the picker's selections into [`GraphPanel::set_relation`].
    _relation_choices: Subscription,
    /// Whether hovering a box marks its neighborhood; flipped by the `h` key or
    /// the toolbar toggle so the marking can be turned off when it is more noise
    /// than help.
    neighborhood_enabled: bool,
    /// Whether the boxes standing for the invocation's critical path wear its
    /// halo.
    critical_path_enabled: bool,
    /// Whether the card naming what the inputs cannot be filtered on is unfolded
    /// past its one-line reading.
    missing_open: bool,
    /// How the card listing the overrides in force is scrolled.
    pins: pins::Pins,
    reveal: reveal::Reveal,
    jumps: reveal::Jumps,
    report: report::Card,
    timecard: timecard::Card,
    /// The dependency paths asked for, which the panel draws in place of its
    /// filter.
    why: why::Why,
    /// The titles the canvas drew, rewritten by every paint and resolved against
    /// a pointer by [`box_at`].
    titles: Vec<TitleTarget>,
    /// The layer drawing the graph; notified through
    /// [`GraphPanel::invalidate_canvas`].
    canvas: Entity<GraphCanvas>,
    legend: Legend,
}

impl GraphPanel {
    fn camera(&self) -> Camera {
        self.framing.camera()
    }

    /// The layout the selected mode draws, or `None` while it is being derived.
    fn drawn(&self) -> Option<Drawn> {
        self.derived.drawn(self.mode)
    }

    /// The zoom a frame resolves `drawn`'s view at. Where that is a map whose
    /// camera awaits its fit, the fit picks the level, so this is the zoom the
    /// fit gives, and `None` while the canvas it fits to is unmeasured.
    fn view_zoom(&self, drawn: &Drawn) -> Option<f32> {
        match drawn {
            Drawn::Map { .. } if !self.framing.fitted() => {
                self.framing.pending_fit(drawn.content())
            }
            _ => Some(self.camera().zoom),
        }
    }

    /// The canonical labels the loaded graph's nodes belong to, which a search
    /// pattern is matched over; `None` until the read lands.
    fn labels(&self, cx: &App) -> Option<Arc<LabelIndex>> {
        self.model
            .read(cx)
            .loaded()
            .map(|loaded| Arc::clone(&loaded.shared.labels))
    }

    /// The boxes the selected mode's layout lets the user open or close.
    fn expandable(&self) -> Expandable {
        self.derived.expandable(self.mode)
    }

    /// How far the drawing being eased onto its successor has travelled at this
    /// instant; `None` at rest, where every box sits at its own layout rect. The
    /// paint and every gesture that names a box place through this one frame, so
    /// what the cursor lands on is what is drawn under it.
    fn morph_frame(&self) -> Option<Frame> {
        self.morph.at(Instant::now())
    }

    fn new(title: SharedString, model: Entity<GraphModel>, cx: &mut Context<Self>) -> Self {
        let mode = LayoutMode::default();
        let mode_picker = cx.new(|cx| {
            Dropdown::new(
                "layout-mode",
                LayoutMode::ALL.map(LayoutMode::label),
                mode.index(),
                cx,
            )
        });
        // The picker notifies on every repaint it makes, and only a selection
        // should move the view off its layout.
        let choices = cx.subscribe(&mode_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(mode) = LayoutMode::at(*index) {
                view.set_mode(mode, cx);
            }
        });
        let colouring = Colouring::default();
        let offered = colour::Offered::by(model.read(cx));
        let colour_picker = cx.new(|cx| {
            let mut picker = Dropdown::new(
                "colouring",
                Colouring::ALL.map(Colouring::label),
                colouring.index(),
                cx,
            );
            colour::offer(&mut picker, offered, cx);
            picker
        });
        let colour_choices = cx.subscribe(&colour_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(colouring) = Colouring::at(*index) {
                colour::set(view, colouring, cx);
            }
        });
        let selection = Selection::none();
        let relation_picker = cx.new(|cx| {
            Dropdown::new(
                "selection-relation",
                Relation::ALL.map(Relation::label),
                selection.relation().index(),
                cx,
            )
        });
        let relations = cx.subscribe(&relation_picker, |view, _, event, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(relation) = Relation::at(*index) {
                view.set_relation(relation, cx);
            }
        });
        let class_filter =
            cx.new(|cx| MultiSelect::new("graph-classes", "classes", Vec::new(), cx));
        let class_choices = cx.subscribe(&class_filter, |view, _, event: &MultiSelectEvent, cx| {
            checklist::choose_classes(view, event, cx);
        });
        let origin_filter =
            cx.new(|cx| MultiSelect::new("graph-origins", "repositories", Vec::new(), cx));
        let origin_choices =
            cx.subscribe(&origin_filter, |view, _, event: &MultiSelectEvent, cx| {
                checklist::choose_origins(view, event, cx);
            });
        let preset_filter =
            cx.new(|cx| MultiSelect::new("graph-presets", "presets", Vec::new(), cx));
        let preset_choices =
            cx.subscribe(&preset_filter, |view, _, event: &MultiSelectEvent, cx| {
                checklist::choose_presets(view, event, cx);
            });
        let search = Search::new(cx);
        let search_typing = cx.subscribe(search.field(), search::typed);
        let parent = cx.entity().downgrade();
        let canvas = cx.new(|_| GraphCanvas { parent });
        let changed = cx.observe(&model, |view, _, cx| {
            colour::sync(view, cx);
            view.invalidate_canvas(cx);
        });
        let clock_moved = model.read(cx).clock().cloned().map(|clock| {
            cx.observe(&clock, |view, _, cx| {
                if view.colouring == Colouring::Playback {
                    view.canvas.update(cx, |_, cx| cx.notify());
                }
            })
        });
        let source_replaced = cx.subscribe(&model, |view, _, _: &SourceReplaced, cx| {
            derive::opened(view, cx);
            checklist::adopt(view, cx);
        });
        let map_placed = cx.subscribe(&model, |view, _, _: &MapPlaced, cx| {
            derive::placed(view, cx)
        });
        let reveal_requested = cx.subscribe(&model, |view, _, event: &RevealRequested, cx| {
            if event.panel == cx.entity_id() {
                match event.reveal {
                    Reveal::Activate => reveal::reveal(view, event.node, cx),
                    Reveal::InPlace => reveal::show_in_place(view, event.node, cx),
                }
            }
        });
        let derived = Derived::new();
        let (panel, filter) = (cx.entity(), derived.filter().clone());
        model.update(cx, |model, cx| model.defer_to(&panel, filter, cx));
        Self {
            focus_handle: cx.focus_handle(),
            title,
            model,
            derived,
            framing: Framing::new(),
            drag: Drag::None,
            emphasis: Emphasis::none(),
            morph: Morph::none(),
            mode,
            mode_picker,
            _mode_choices: choices,
            colouring,
            colour_picker,
            _colour_choices: colour_choices,
            classes: Vec::new(),
            class_filter,
            _class_choices: class_choices,
            origins: Vec::new(),
            origin_filter,
            _origin_choices: origin_choices,
            presets: Vec::new(),
            preset_filter,
            _preset_choices: preset_choices,
            search,
            jump: jump::Finder::default(),
            _search_typing: search_typing,
            _changed: changed,
            _clock_moved: clock_moved,
            _source_replaced: source_replaced,
            _map_placed: map_placed,
            _reveal_requested: reveal_requested,
            hovered: None,
            selection,
            relation_picker,
            _relation_choices: relations,
            neighborhood_enabled: true,
            critical_path_enabled: true,
            missing_open: false,
            pins: pins::Pins::new(),
            reveal: reveal::Reveal::Idle,
            jumps: reveal::Jumps::default(),
            report: report::Card::new(),
            timecard: timecard::Card::new(),
            why: why::Why::new(),
            titles: Vec::new(),
            canvas,
            legend: Legend::default(),
        }
    }

    /// Rebuilds the graph scene along with the frame around it; every change to
    /// what the canvas draws routes through here, and a change that leaves the
    /// drawing alone notifies alone, so the scene replays from its cache.
    pub(crate) fn invalidate_canvas(&self, cx: &mut Context<Self>) {
        self.canvas.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Brings this drawing onto a layered layout the graph has landed.
    ///
    /// A morph eases one layered drawing onto its successor, so it says nothing
    /// about a map's levels, which the camera alone selects between: armed while
    /// the map is on screen it would pump repaints of a canvas that draws none of
    /// it.
    pub(crate) fn settle_onto(&mut self, landed: &LayoutLanded, cx: &mut Context<Self>) {
        let replaced = landed
            .replaced
            .clone()
            .filter(|_| self.mode == LayoutMode::Layered);
        if let Some(replaced) = replaced {
            morph::begin(self, replaced, &landed.current, cx);
        }
        self.framing.request_recenter();
        emphasis::begin_fade(self, cx);
    }

    /// Frames the layer drawing the selected mode's layout, or a spinner while
    /// that layout is still being computed — the toolbar stays above either, so
    /// the picker remains reachable.
    fn viewport(
        &mut self,
        palette: Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let laying_out = || bviz_ui_components::centered_spinner("Laying out…", &palette.colors);
        let Some(drawn) = self.drawn() else {
            let entity = cx.entity();
            return laying_out()
                .relative()
                .child(overlay_canvas(move |bounds, _, cx| {
                    entity.update(cx, |view, _| view.framing.measure(bounds));
                }))
                .into_any_element();
        };
        let zoom = self.view_zoom(&drawn);
        let underived = zoom.is_some_and(|zoom| drawn.view(zoom).is_none());
        let loaded = self.model.read(cx).loaded();
        let legend = zoom.zip(loaded).and_then(|(zoom, loaded)| {
            let (statuses, resolved) = (loaded.statuses(), loaded.taken_up());
            let coloured = legend::Coloured {
                colouring: self.colouring,
                times: loaded.times().ok(),
                timed: loaded.timed(),
                states: loaded.states(),
                attempted: loaded.attempted(),
            };
            let haloed = legend::Haloed {
                nodes: self.critical_path_enabled.then(|| loaded.critical()),
                traced: loaded.traced(),
            };
            self.legend
                .card(&drawn, zoom, statuses, resolved, coloured, haloed, &palette)
        });
        let entity = cx.entity();
        div()
            .id("graph-viewport")
            .relative()
            .size_full()
            .overflow_hidden()
            .child(cached_layer(self.canvas.clone()))
            .children(underived.then(|| div().absolute().size_full().flex().child(laying_out())))
            .child(
                // What the drawing itself cannot state, held clear of the graph
                // at the foot of the canvas.
                div()
                    .absolute()
                    .left_2()
                    .bottom_2()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_2()
                    .children(missing::card(self, &palette, cx))
                    .children(legend),
            )
            .child(
                column::Column::new()
                    .held(reveal::card(self, &palette, cx))
                    .held(pins::panel(self, &palette, cx))
                    .folding(report::card(self, &palette, cx))
                    .folding(timecard::card(self, &palette, cx))
                    .foot(why::card(self, &palette, cx)),
            )
            .children(search::card(self, &palette, cx))
            .children(jump::overlay(self, &palette, window, cx))
            // A replayed scene installs the handlers recorded with it, which a
            // press's own frame has yet to make; the panel's own paint is what
            // installs them for the gesture that press begins.
            .child(overlay_canvas(move |_, window, cx| {
                track_gesture(&entity, window, cx)
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_secondary_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_hover(cx.listener(Self::on_hover))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            // Track the shared focus handle here too (the chrome already tracks
            // it on an ancestor) so a press on the canvas brings the focus back
            // here from whichever control last took it. Key events bubble from
            // here up through the strip's own bindings to the chrome's.
            .track_focus(&self.focus_handle)
            .into_any_element()
    }

    /// Switches which layout is drawn, starting the newly selected one's if it
    /// has not been computed yet.
    fn set_mode(&mut self, mode: LayoutMode, cx: &mut Context<Self>) {
        if self.switch_mode(mode, cx) {
            derive::ensure_layout(self, cx);
            self.invalidate_canvas(cx);
        }
    }

    /// Switches which layout is drawn, leaving it to the caller to derive what
    /// that mode draws; reports whether the mode changed. The two span
    /// different extents, so the view refits.
    ///
    /// The hover, the emphasis and the morph all describe the layout being left:
    /// an emphasis carried across would let a relayout still in flight settle the
    /// incoming view onto a box picked out in the other one, and highlight it,
    /// and a morph would ease boxes of the layout left behind over the one
    /// arriving.
    fn switch_mode(&mut self, mode: LayoutMode, cx: &mut Context<Self>) -> bool {
        if self.mode == mode {
            return false;
        }
        self.mode = mode;
        self.mode_picker
            .update(cx, |picker, cx| picker.set_selected(mode.index(), cx));
        self.hovered = None;
        self.emphasis.clear();
        self.morph.clear();
        self.framing.refit();
        true
    }

    fn on_mouse_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        // The card and the palette occlude the canvas, so a press reaching here
        // landed outside them.
        search::dismiss(self, cx);
        jump::dismiss(self, cx);
        // A press on a scroll-bar gutter drives the thumb instead of panning the
        // content under it.
        if let Some(axis) = scrollbar_hit(self, e.position) {
            start_scroll_drag(self, axis, e.position, cx);
            return;
        }
        start_pan(self, e.position, cx);
    }

    /// Picks the box under the cursor out for as long as it stays selected, or
    /// drops the selection where the press lands on no box.
    fn on_secondary_down(&mut self, e: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        search::dismiss(self, cx);
        jump::dismiss(self, cx);
        select_box_at(self, e.position, cx);
    }

    /// Ends the drag on the release the element receives directly, which the
    /// window-global handlers can miss: they are installed only once a frame
    /// paints with a drag in progress, so a press-release quick enough to finish
    /// before that paint would leave the drag running.
    fn on_mouse_up(&mut self, e: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        end_drag(self, e.position, DragEnd::Released, window, cx);
    }

    fn on_mouse_move(&mut self, e: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        // A pan moves the view under a stationary pointer, so the box it is over
        // is the one already resolved.
        if matches!(self.drag, Drag::Pan(_)) {
            return;
        }
        let Some(drawn) = self.drawn() else {
            return;
        };
        let camera = self.camera();
        let vp = self.framing.viewport();
        let morph = self.morph_frame();
        let hit = drawn
            .view(camera.zoom)
            .and_then(|g| box_at(&self.titles, g, morph.as_ref(), vp, camera, e.position))
            .map(|b| b.id);
        if hit != self.hovered {
            self.hovered = hit;
            self.invalidate_canvas(cx);
        }
    }

    /// Drops the hover when the pointer leaves the canvas, so a node's
    /// neighborhood does not stay marked while the cursor is over the toolbar or
    /// the window chrome — the move handler is bounds-gated and never fires there.
    /// gpui reports the canvas unhovered while a button is held, so the drag guard
    /// keeps a pan from dropping the marked set it started with.
    fn on_hover(&mut self, over: &bool, _: &mut Window, cx: &mut Context<Self>) {
        if !over && matches!(self.drag, Drag::None) && self.hovered.take().is_some() {
            self.invalidate_canvas(cx);
        }
    }

    fn on_scroll_wheel(&mut self, e: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(content) = self.drawn().map(|d| d.content()) else {
            return;
        };
        let notches = match e.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / PIXELS_PER_NOTCH,
        };
        let viewport = self.framing.viewport();
        self.framing.move_to(zoom_step(
            e.position,
            self.camera(),
            notches,
            viewport,
            content,
        ));
        self.invalidate_canvas(cx);
    }

    /// Puts away the innermost thing the reader set up: a reveal pending, then
    /// the selection, and with neither the dependency paths asked for.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        if reveal::dismiss(self) {
            cx.notify();
        } else if self.selection.node().is_some() {
            self.selection.clear();
            self.invalidate_canvas(cx);
        } else {
            why::clear(self, cx);
        }
    }

    fn toggle_neighborhood(&mut self, cx: &mut Context<Self>) {
        self.neighborhood_enabled = !self.neighborhood_enabled;
        self.invalidate_canvas(cx);
    }

    fn toggle_critical_path(&mut self, cx: &mut Context<Self>) {
        self.critical_path_enabled = !self.critical_path_enabled;
        self.invalidate_canvas(cx);
    }

    /// Widens or narrows what the selected box marks.
    fn set_relation(&mut self, relation: Relation, cx: &mut Context<Self>) {
        self.selection.set_relation(relation);
        self.invalidate_canvas(cx);
    }

    fn toggle_reduce(&mut self, cx: &mut Context<Self>) {
        if !self.mode.honors_reduce() || self.derived.reduced().is_none() {
            return;
        }
        derive::toggle_reduce(self, cx);
    }
}

impl EventEmitter<PanelEvent> for GraphPanel {}

impl Focusable for GraphPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Panel for GraphPanel {
    fn kind() -> PanelKind {
        PanelKind {
            view: ViewKind::new("graph"),
            label: "Graph",
            key_context: KEY_CONTEXT,
        }
    }

    fn title(&self, _cx: &App) -> SharedString {
        self.title.clone()
    }

    fn status(&self, cx: &App) -> Option<SharedString> {
        Some(
            derive::status(
                self.model.read(cx).source(),
                self.drawn()
                    .and_then(|drawn| self.view_zoom(&drawn).map(|zoom| (drawn, zoom))),
                self.derived.over_budget(),
            )
            .into(),
        )
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.focus_handle.focus(window, cx);
            derive::publish_filter(self, cx);
        }
    }
}

impl Render for GraphPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        search::watch_field(self, window, cx);
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let chrome = div()
            .key_context(Self::kind().key_context)
            .on_action(
                cx.listener(|view, _: &ToggleNeighborhood, _, cx| view.toggle_neighborhood(cx)),
            )
            .on_action(cx.listener(|view, _: &ToggleReduce, _, cx| view.toggle_reduce(cx)))
            .on_action(cx.listener(|view, _: &Dismiss, _, cx| view.dismiss(cx)))
            .on_action(cx.listener(|view, _: &JumpBack, _, cx| {
                jump::dismiss(view, cx);
                reveal::back(view, cx);
            }))
            .on_action(cx.listener(|view, _: &JumpForward, _, cx| {
                jump::dismiss(view, cx);
                reveal::forward(view, cx);
            }))
            .on_action(cx.listener(|view, _: &jump::Open, window, cx| jump::open(view, window, cx)))
            .track_focus(&self.focus_handle)
            .flex_1()
            .flex()
            .flex_col()
            .overflow_hidden();
        let placeholder = self.model.read(cx).source().placeholder(&palette.colors);
        match placeholder {
            Some(placeholder) => chrome.child(placeholder),
            None => {
                let traced = self
                    .model
                    .read(cx)
                    .loaded()
                    .is_some_and(|loaded| loaded.critical().any());
                let controls = Controls {
                    mode: self.mode,
                    search: search::field(&self.search, cx).into_any_element(),
                    picker: self.mode_picker.clone(),
                    class_filter: (!self.classes.is_empty()).then(|| self.class_filter.clone()),
                    origin_filter: (!self.origins.is_empty()).then(|| self.origin_filter.clone()),
                    preset_filter: self.preset_filter.clone(),
                    relation_picker: self.selection.node().map(|_| self.relation_picker.clone()),
                    pin_control: pins::control(self, cx).map(IntoElement::into_any_element),
                    why_controls: why::controls(self, cx).map(IntoElement::into_any_element),
                    colour_picker: colour::control(self, &palette.colors, cx).into_any_element(),
                    report_control: report::control(self, cx).map(IntoElement::into_any_element),
                    timecard_control: timecard::control(self, cx)
                        .map(IntoElement::into_any_element),
                    neighborhood_enabled: self.neighborhood_enabled,
                    critical_path: traced.then_some(self.critical_path_enabled),
                    reduced: self.derived.reduced(),
                    relaying: self.derived.deriving(),
                };
                chrome
                    .child(toolbar::toolbar(controls, &palette, cx))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .overflow_hidden()
                            .child(self.viewport(palette, window, cx)),
                    )
            }
        }
    }
}

/// The layer the graph is drawn in, so that a frame drawn for any other reason
/// replays it rather than laying the drawing out again.
struct GraphCanvas {
    /// What the drawing is made from, and what its prepaint and paint write
    /// back through.
    parent: WeakEntity<GraphPanel>,
}

impl Render for GraphCanvas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(parent) = self.parent.upgrade() else {
            return div().into_any_element();
        };
        let palette = Palette::from_theme(&bviz_ui_theme::theme(window, cx));
        let view = parent.read(cx);
        let Some(drawn) = view.drawn() else {
            return div().into_any_element();
        };
        let clock = view.model.read(cx).clock();
        let at = clock.and_then(|clock| clock.read(cx).now());
        let playing = clock.filter(|_| view.colouring == Colouring::Playback);
        Clock::keep_pace(playing, window, cx);
        let content = drawn.content();
        let recenter_target = match view.view_zoom(&drawn).and_then(|zoom| drawn.view(zoom)) {
            None => Target::Pending,
            Some(g) => Target::Resolved(recenter_rect(g, &view.emphasis)),
        };
        let scene = Scene {
            expandable: view.expandable(),
            palette,
            faded: view.derived.faded(),
            hovered: view.hovered.filter(|_| view.neighborhood_enabled),
            selected: view.selection.node(),
            highlight: view.emphasis.highlight(),
            colouring: view.colouring,
            at,
            critical_path: view.critical_path_enabled,
            drawn,
        };
        let prepaint = build_prepaint(parent.clone(), content, recenter_target);
        canvas(prepaint, canvas_body(scene, parent))
            .size_full()
            .into_any_element()
    }
}

/// What the toolbar's controls show.
struct Controls {
    mode: LayoutMode,
    /// The field a label pattern is typed into, with the button that empties it.
    search: AnyElement,
    picker: Entity<Dropdown>,
    /// The checklists over the facets the loaded graph carries, each offered
    /// only where that graph carries a value on its axis.
    class_filter: Option<Entity<MultiSelect>>,
    origin_filter: Option<Entity<MultiSelect>>,
    /// The reductions offered as one click, listed whether or not the loaded
    /// inputs can answer each, so that one they cannot says why.
    preset_filter: Entity<MultiSelect>,
    /// The picker naming what the selected box marks, offered only while a box
    /// is selected: with none there is no marking for it to widen or narrow.
    relation_picker: Option<Entity<Dropdown>>,
    /// The override offered on the selected box, which stands beside that
    /// picker.
    pin_control: Option<AnyElement>,
    /// The questions asked of the selected box, which stand beside the pin.
    why_controls: Option<AnyElement>,
    /// Chooses what the boxes' fills say.
    colour_picker: AnyElement,
    /// Puts the card reading what the build reported up and away, offered
    /// where the build reported anything.
    report_control: Option<AnyElement>,
    /// Puts the card saying where the profile's action time went up and
    /// away, offered where a profile places time on the graph.
    timecard_control: Option<AnyElement>,
    neighborhood_enabled: bool,
    /// Whether the critical path's halo is drawn, offered only where a path
    /// lands on the graph.
    critical_path: Option<bool>,
    /// `None` where the drawing offers no choice of edge set.
    reduced: Option<bool>,
    relaying: bool,
}

/// The box a pointer at `pos` (window px) resolves to, which every gesture that
/// names a box — the hover that marks a neighborhood, the click that opens or
/// closes one — goes through, so the box under the cursor is the same box for
/// all of them.
///
/// A title the frame painted wins over the box the cursor falls in, because a
/// title the zoom has floated out of its container's band is drawn over the
/// boxes inside it: there the innermost box under the cursor is one of those,
/// and the opaque title covering it is both what the user sees and what the
/// user is aiming at. A title is taken only where the box it names is drawn over
/// the cursor in `g` under `camera` and `morph`. Since a title is always painted
/// within its own box, that leaves a set recorded from a frame the view has moved
/// on from — a relayout, a mode switch, a camera step — resolving to the box
/// under the cursor instead.
fn box_at<'a>(
    titles: &[TitleTarget],
    g: &'a bviz_graph_layout::LaidOut,
    morph: Option<&Frame>,
    viewport: Bounds<Pixels>,
    camera: Camera,
    pos: Point<Pixels>,
) -> Option<&'a bviz_graph_layout::Box> {
    let (gx, gy) = from_screen(pos, viewport.origin, camera);
    let titled = paint::title_at(titles, pos)
        .and_then(|id| g.boxes.iter().find(|b| b.id == id))
        .filter(|b| placed_rect(b, morph).covers(gx, gy));
    titled.or_else(|| hit_box(&g.boxes, morph, gx, gy))
}

/// Opens or closes the box under `pos` (window px) and settles the view to keep
/// it in frame. A box the drawn layout holds no handle on, or a `pos` off the
/// canvas, is left alone.
fn click_box_at(view: &mut GraphPanel, pos: Point<Pixels>, cx: &mut Context<GraphPanel>) {
    let vp = view.framing.viewport();
    if !vp.contains(&pos) {
        return;
    }
    let camera = view.camera();
    let Some(drawn) = view.drawn() else {
        return;
    };
    let Some(g) = drawn.view(camera.zoom) else {
        return;
    };
    let morph = view.morph_frame();
    let expandable = view.expandable();
    let hit =
        box_at(&view.titles, g, morph.as_ref(), vp, camera, pos).filter(|b| expandable.holds(b));
    let Some(id) = hit.map(|b| b.id) else {
        return;
    };
    // The relayout places the boxes afresh, so the box resolved here need not be
    // the one the cursor ends up over.
    view.hovered = None;
    derive::toggle_expansion(view, id, cx);
    view.emphasis.toggled(id);
    view.invalidate_canvas(cx);
}

/// Selects the box under `pos` (window px), or drops the selection where `pos`
/// lands on no box or off the canvas. Selecting the already-selected box drops it
/// too, so the same gesture puts the marking away.
fn select_box_at(view: &mut GraphPanel, pos: Point<Pixels>, cx: &mut Context<GraphPanel>) {
    let vp = view.framing.viewport();
    let camera = view.camera();
    let morph = view.morph_frame();
    let hit = view
        .drawn()
        .filter(|_| vp.contains(&pos))
        .and_then(|drawn| {
            let g = drawn.view(camera.zoom)?;
            box_at(&view.titles, g, morph.as_ref(), vp, camera, pos).map(|b| b.id)
        });
    match hit {
        Some(id) => view.selection.toggle(id),
        None => view.selection.clear(),
    }
    view.invalidate_canvas(cx);
}

/// Where `b` is drawn: its own layout rect at rest, and the rect `morph` has
/// eased it to while one drawing settles onto another.
fn placed_rect(b: &bviz_graph_layout::Box, morph: Option<&Frame>) -> Rect {
    morph.map_or(Rect::of(b), |frame| frame.place(b).0)
}

/// The smallest-area box (in layout space) drawn over `(gx, gy)`, so an inner
/// member or header wins over its enclosing container; `None` if outside all.
/// The boxes a morph is fading out are absent from `boxes`, so a gesture passes
/// through them to whatever the new drawing puts there.
fn hit_box<'a>(
    boxes: &'a [bviz_graph_layout::Box],
    morph: Option<&Frame>,
    gx: f32,
    gy: f32,
) -> Option<&'a bviz_graph_layout::Box> {
    boxes
        .iter()
        .map(|b| (b, placed_rect(b, morph)))
        .filter(|(_, rect)| rect.covers(gx, gy))
        .min_by(|(_, a), (_, b)| (a.w * a.h).total_cmp(&(b.w * b.h)))
        .map(|(b, _)| b)
}

/// A joint graph being read and laid out.
pub struct Load {
    built: Pending<model::Read>,
    title: SharedString,
}

impl From<GraphSource> for Load {
    fn from(source: GraphSource) -> Self {
        let title = title(&source);
        Self {
            built: Pending::start(move || model::build(source)),
            title,
        }
    }
}

/// Names the file the graph was read from, and how many more it merges.
fn title(source: &GraphSource) -> SharedString {
    let (first, rest) = source.first_and_rest();
    match rest.len() {
        0 => document_label(&first.path),
        more => format!("{} +{more}", document_label(&first.path)).into(),
    }
}

/// The views drawing the graph `load` reads, which tells `session` what the
/// graph knows of the invocation where it is opened as part of one.
pub fn open(
    load: impl Into<Load>,
    session: Option<Entity<Session>>,
    clock: Option<DocumentClock>,
    _window: &mut Window,
    cx: &mut App,
) -> Member {
    let load = load.into();
    let label = load.title;
    let model = cx.new(|cx| GraphModel::reading(load.built, session, clock, cx));
    let graph = ViewSpec::new({
        let (label, model) = (label.clone(), model.clone());
        move |cx| cx.new(|cx| GraphPanel::new(label.clone(), model.clone(), cx))
    });
    let scatter = ViewSpec::new({
        let model = model.clone();
        move |cx| cx.new(|cx| ScatterPanel::new(model.clone(), cx))
    });
    let exclusive = ViewSpec::new(move |cx| cx.new(|cx| ExclusivePanel::new(model.clone(), cx)));
    Member::new(
        label,
        PanelGroup::tabs(graph, vec![scatter, exclusive]).with_weight(Weight::CHART),
    )
}

/// Renders a Bazel build graph from one or more `--output=streamed_proto`
/// files. Each flag selects a file and the query that produced it; passing
/// several merges them into one joint graph, with shared entities collapsed.
#[derive(Parser)]
#[command(name = "graph", version, about)]
struct Cli {
    #[command(flatten)]
    graph: GraphArgs,
}

/// Parses the query inputs and opens the graph-viewer window.
pub fn run() {
    let source = Cli::parse().graph.source().unwrap_or_else(|| {
        Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "one of --aquery, --cquery or --query is required",
            )
            .exit()
    });
    bviz_ui_workspace::run_panels(
        "bviz graph",
        size(px(1280.), px(820.)),
        Load::from(source),
        Vec::new(),
        |load: Load, window, cx| open(load, None, None, window, cx).into(),
        &[register_keys],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::{
        Rect, clamp_pan, ease_out_cubic, fit_view, fit_zoom, settle_view, to_screen,
    };
    use crate::paint::lane_polyline;
    use bviz_graph_layout::Box;
    use gpui::{Bounds, Size, point};

    fn lbox(id: u32, x: f64, y: f64, w: f64, h: f64) -> Box {
        Box {
            id: bviz_graph_layout::Id(id),
            label: String::new(),
            kind: Kind::Source,
            x,
            y,
            width: w,
            height: h,
            compound: false,
            group: false,
        }
    }

    /// The action `keys` dispatch with focus in the innermost of `contexts`,
    /// under the panel's bindings and those of the text field.
    fn dispatched(keys: &str, contexts: &[&str]) -> Option<std::boxed::Box<dyn gpui::Action>> {
        let mut bindings = bindings();
        bindings.extend(text_input::bindings());
        let keymap = gpui::Keymap::new(bindings);
        let stack: Vec<gpui::KeyContext> = contexts
            .iter()
            .map(|context| gpui::KeyContext::parse(context).expect("a key context"))
            .collect();
        let keystroke = gpui::Keystroke::parse(keys).expect("a keystroke");
        let (matched, _) = keymap.bindings_for_input(&[keystroke], &stack);
        matched
            .first()
            .map(|binding| binding.action().boxed_clone())
    }

    fn is<A: gpui::Action>(action: Option<std::boxed::Box<dyn gpui::Action>>, expected: A) -> bool {
        action.is_some_and(|action| action.partial_eq(&expected))
    }

    const IN_PANEL: &[&str] = &[KEY_CONTEXT];
    const IN_FIELD: &[&str] = &[KEY_CONTEXT, text_input::KEY_CONTEXT];
    const IN_PALETTE: &[&str] = &[KEY_CONTEXT, jump::KEY_CONTEXT, text_input::KEY_CONTEXT];
    const IN_SEARCH: &[&str] = &[KEY_CONTEXT, search::KEY_CONTEXT, text_input::KEY_CONTEXT];

    /// A letter typed into any text field is text, not one of the panel's
    /// keys.
    #[test]
    fn the_panels_letters_stand_down_in_any_text_field() {
        assert!(is(dispatched("h", IN_PANEL), ToggleNeighborhood));
        assert!(is(dispatched("r", IN_PANEL), ToggleReduce));
        for field in [IN_SEARCH, IN_FIELD] {
            assert!(dispatched("h", field).is_none());
            assert!(dispatched("r", field).is_none());
        }
    }

    #[test]
    fn slash_opens_the_palette_only_where_it_types_nothing() {
        assert!(is(dispatched("/", IN_PANEL), jump::Open));
        assert!(dispatched("/", IN_SEARCH).is_none());
        assert!(dispatched("/", IN_PALETTE).is_none());
        for find in ["ctrl-f", "cmd-f"] {
            assert!(is(dispatched(find, IN_PANEL), jump::Open), "{find}");
            assert!(is(dispatched(find, IN_SEARCH), jump::Open), "{find}");
        }
    }

    /// A letter typed into the palette's field is text, and the keys moving
    /// through its rows reach it before the panel's own.
    #[test]
    fn the_palette_takes_its_keys_before_the_panel() {
        assert!(dispatched("h", IN_PALETTE).is_none());
        assert!(is(dispatched("h", IN_PANEL), ToggleNeighborhood));
        assert!(is(dispatched("escape", IN_PALETTE), jump::Close));
        assert!(is(dispatched("escape", IN_PANEL), Dismiss));
        assert!(is(dispatched("up", IN_PALETTE), jump::Previous));
        assert!(is(dispatched("enter", IN_PALETTE), jump::Go));
        assert!(is(dispatched("ctrl-enter", IN_PALETTE), jump::AskWhy));
        assert!(is(dispatched("tab", IN_PALETTE), jump::NextScope));
        assert!(is(dispatched("shift-tab", IN_PALETTE), jump::PreviousScope));
    }

    /// The panel's own jumps and the exclusive view's foci are walked by the
    /// same keys, each in its own panel, and a text field keeps them.
    #[test]
    fn alt_arrows_walk_the_history_of_the_panel_holding_focus() {
        assert!(is(dispatched("alt-left", IN_PANEL), JumpBack));
        assert!(is(dispatched("alt-right", IN_PANEL), JumpForward));
        for keys in ["alt-left", "alt-right"] {
            for field in [IN_PALETTE, IN_SEARCH] {
                assert!(!is(dispatched(keys, field), JumpBack), "{keys}");
                assert!(!is(dispatched(keys, field), JumpForward), "{keys}");
            }
        }
        let exclusive = &[exclusive::KEY_CONTEXT];
        assert!(is(dispatched("alt-left", exclusive), exclusive::FocusBack));
        assert!(is(
            dispatched("alt-right", exclusive),
            exclusive::FocusForward
        ));
    }

    #[test]
    fn from_screen_inverts_to_screen() {
        let bounds = Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.)));
        let camera = Camera {
            zoom: 1.7,
            pan: (33.0, -12.0),
        };
        for &(x, y) in &[(0.0_f64, 0.0_f64), (42.5, 7.25), (123.0, 456.0)] {
            let screen = to_screen(x, y, bounds, camera);
            let (gx, gy) = from_screen(screen, bounds.origin, camera);
            assert!((gx as f64 - x).abs() < 1e-3, "x: {gx} vs {x}");
            assert!((gy as f64 - y).abs() < 1e-3, "y: {gy} vs {y}");
        }
    }

    #[test]
    fn an_emphasis_holds_a_target_in_whichever_layout_is_drawn() {
        // A box the emphasis names is named by graph id, which every layout
        // shares, so one left over from the layout that produced it still
        // resolves against the one switched to — and would settle the fresh view
        // onto it. Clearing it, as a mode switch does, leaves nothing to settle.
        let mut emphasis = Emphasis::none();
        emphasis.toggled(bviz_graph_layout::Id(7));
        let g = bviz_graph_layout::LaidOut {
            boxes: vec![lbox(7, 10.0, 20.0, 30.0, 40.0)],
            ..Default::default()
        };
        assert!(recenter_rect(&g, &emphasis).is_some());
        emphasis.clear();
        assert!(recenter_rect(&g, &emphasis).is_none());
    }

    #[test]
    fn a_settle_frames_the_toggled_box_and_not_what_sits_around_it() {
        // A toggled box with a neighbour far off to the side. Framing the pair
        // would bound a rect wide enough to force a zoom-out, and an expansion
        // parts neighbours like this every time; the settle takes the toggled
        // box alone.
        let mut emphasis = Emphasis::none();
        emphasis.toggled(bviz_graph_layout::Id(7));
        let g = bviz_graph_layout::LaidOut {
            boxes: vec![
                lbox(7, 10.0, 20.0, 30.0, 40.0),
                lbox(1, 5000.0, 4000.0, 30.0, 40.0),
            ],
            ..Default::default()
        };
        let rect = recenter_rect(&g, &emphasis).expect("the toggled box is drawn");
        assert_eq!(
            (rect.x, rect.y, rect.w, rect.h),
            (10.0, 20.0, 30.0, 40.0),
            "the settle reached past the toggled box"
        );
        // And so the settle that rect feeds leaves the reader's zoom alone, where
        // one bounding the neighbour too would have to zoom out to enclose it.
        let (z, _) = settle_view(rect, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0), 1.0);
        assert!((z - 1.0).abs() < 1e-4, "the toggle moved the zoom to {z}");
    }

    #[test]
    fn hit_box_picks_innermost_and_none_outside() {
        // An outer container (id 0) with a small inner box (id 1) nested in it.
        let boxes = vec![
            lbox(0, 0.0, 0.0, 100.0, 100.0),
            lbox(1, 20.0, 20.0, 30.0, 30.0),
        ];
        // Inside both rects: the smaller (inner) wins.
        assert_eq!(
            hit_box(&boxes, None, 30.0, 30.0).unwrap().id,
            bviz_graph_layout::Id(1)
        );
        // Inside only the container.
        assert_eq!(
            hit_box(&boxes, None, 5.0, 5.0).unwrap().id,
            bviz_graph_layout::Id(0)
        );
        // Outside everything.
        assert!(hit_box(&boxes, None, 200.0, 200.0).is_none());
        assert!(hit_box(&boxes, None, -1.0, 50.0).is_none());
    }

    #[test]
    fn lane_polyline_offsets_every_vertex_in_parallel() {
        // An L route: right to (100,0), then down to (100,100).
        let lane = lane_polyline(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)], 4.0);
        // Endpoints are pushed perpendicular too (not anchored), so the first and
        // last segments stay parallel to the route and meet the node offset.
        assert!(
            (lane[0].0).abs() < 0.01 && (lane[0].1 - 4.0).abs() < 0.5,
            "start {:?}",
            lane[0]
        );
        assert!(
            (lane[2].0 - 96.0).abs() < 0.5 && (lane[2].1 - 100.0).abs() < 0.01,
            "end {:?}",
            lane[2]
        );
        // First segment horizontal like the route's (no fanning in at the node).
        assert!(
            (lane[0].1 - lane[1].1).abs() < 0.01,
            "first seg not parallel: {lane:?}"
        );
        // Last segment vertical like the route's.
        assert!(
            (lane[1].0 - lane[2].0).abs() < 0.01,
            "last seg not parallel: {lane:?}"
        );
    }

    #[test]
    fn lane_polyline_zero_offset_is_identity() {
        let route = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)];
        assert_eq!(lane_polyline(&route, 0.0), route);
    }

    #[test]
    fn lane_polyline_opposite_signs_nest_on_opposite_sides() {
        let route = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)];
        let plus = lane_polyline(&route, 4.0)[1];
        let minus = lane_polyline(&route, -4.0)[1];
        // The two lanes never cross: their corners fall on opposite sides.
        assert_ne!(plus.1.signum(), minus.1.signum(), "{plus:?} {minus:?}");
    }

    #[test]
    fn lane_polyline_parallel_shifts_a_straight_run() {
        // A straight two-point edge shifts whole, both ends offset perpendicular.
        let lane = lane_polyline(&[(0.0, 0.0), (100.0, 0.0)], 4.0);
        assert_eq!(lane.len(), 2);
        assert!(
            (lane[0].0).abs() < 0.01 && (lane[0].1 - 4.0).abs() < 0.5,
            "{lane:?}"
        );
        assert!(
            (lane[1].0 - 100.0).abs() < 0.01 && (lane[1].1 - 4.0).abs() < 0.5,
            "{lane:?}"
        );
    }

    // The whole graph, large enough that fit_zoom's floor stays well below 1.0 and
    // never bites in these cases.
    const CONTENT: (f32, f32) = (4000.0, 3000.0);

    fn vp(w: f32, h: f32) -> Size<Pixels> {
        size(px(w), px(h))
    }

    /// Asserts `target`'s screen rect under `(zoom, pan)` lies fully within the
    /// viewport, inset by the breathing-room pad (with a small tolerance).
    fn assert_enclosed(target: Rect, vw: f32, vh: f32, zoom: f32, pan: (f32, f32)) {
        let (bx, by, bw, bh) = (
            target.x as f32,
            target.y as f32,
            target.w as f32,
            target.h as f32,
        );
        let x0 = (bx + MARGIN as f32) * zoom + pan.0;
        let y0 = (by + MARGIN as f32) * zoom + pan.1;
        let x1 = (bx + bw + MARGIN as f32) * zoom + pan.0;
        let y1 = (by + bh + MARGIN as f32) * zoom + pan.1;
        let eps = 0.5;
        let (lo, hix, hiy) = (
            RECENTER_PAD - eps,
            vw - RECENTER_PAD + eps,
            vh - RECENTER_PAD + eps,
        );
        assert!(
            x0 >= lo && x1 <= hix,
            "x out of view: {x0}..{x1} in 0..{vw}"
        );
        assert!(
            y0 >= lo && y1 <= hiy,
            "y out of view: {y0}..{y1} in 0..{vh}"
        );
    }

    #[test]
    fn settle_view_leaves_a_fitting_centered_target_put() {
        // A small rect already comfortably inside: neither zoom nor pan moves.
        let target = Rect {
            x: 375.0,
            y: 280.0,
            w: 50.0,
            h: 40.0,
        };
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0), 1.0);
        assert!((z - 1.0).abs() < 1e-4, "zoom moved: {z}");
        assert!(p.0.abs() < 1e-3 && p.1.abs() < 1e-3, "pan moved: {p:?}");
    }

    #[test]
    fn settle_view_only_zooms_out_to_enclose_a_big_target() {
        // A rect larger than the viewport at the current zoom forces a zoom-out,
        // never below the whole-graph fit, and ends up fully framed.
        let target = Rect {
            x: 0.0,
            y: 0.0,
            w: 2000.0,
            h: 1500.0,
        };
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0), 1.0);
        assert!(z < 1.0, "expected zoom-out, got {z}");
        assert!(
            z >= fit_zoom(vp(800., 600.), CONTENT) - 1e-4,
            "below whole-graph fit: {z}"
        );
        assert_enclosed(target, 800., 600., z, p);
    }

    #[test]
    fn settle_view_never_zooms_in_past_the_readers_own_zoom() {
        // A box far smaller than the window does not blow up to fill it: the
        // settle rises no further than the zoom the reader set.
        let target = Rect {
            x: 100.0,
            y: 100.0,
            w: 30.0,
            h: 20.0,
        };
        let (z, _) = settle_view(target, vp(800., 600.), CONTENT, 0.5, (0.0, 0.0), 0.5);
        assert!(z <= 0.5 + 1e-4, "zoomed in: {z}");
    }

    #[test]
    fn opening_one_box_after_another_leaves_the_zoom_alone_while_each_one_fits() {
        // A run of expansions, each landing a box that still fits the window. The
        // view holds the reader's zoom throughout: it is the boxes around the
        // toggled one that an expansion spreads out, so chasing those would step
        // the zoom further out at every toggle.
        let chosen = 1.0;
        let mut camera = (chosen, (0.0, 0.0));
        for (i, along) in [200.0, 900.0, 1600.0, 2300.0].into_iter().enumerate() {
            let opened = Rect {
                x: along,
                y: along / 2.,
                w: 300.0,
                h: 220.0,
            };
            camera = settle_view(opened, vp(800., 600.), CONTENT, camera.0, camera.1, chosen);
            assert!(
                (camera.0 - chosen).abs() < 1e-4,
                "expansion {i} moved the zoom to {}",
                camera.0
            );
        }
    }

    #[test]
    fn settling_over_and_over_neither_walks_the_zoom_out_nor_strands_it() {
        // Expanding grows the toggled box past the window, so the view relaxes to
        // show it whole; collapsing shrinks it back within the window, so the view
        // returns to the zoom the reader set. Every repetition of the pair settles
        // at the same two zooms rather than starting from where the last left off.
        let chosen = 1.0;
        let opened = Rect {
            x: 0.0,
            y: 0.0,
            w: 2000.0,
            h: 1500.0,
        };
        let shut = Rect {
            x: 375.0,
            y: 280.0,
            w: 50.0,
            h: 40.0,
        };
        let mut camera = (chosen, (0.0, 0.0));
        let mut settled = Vec::new();
        for _ in 0..4 {
            camera = settle_view(opened, vp(800., 600.), CONTENT, camera.0, camera.1, chosen);
            let relaxed = camera.0;
            camera = settle_view(shut, vp(800., 600.), CONTENT, camera.0, camera.1, chosen);
            settled.push((relaxed, camera.0));
        }
        assert!(
            settled[0].0 < chosen,
            "the open target did not relax the view"
        );
        for (i, &(relaxed, back)) in settled.iter().enumerate() {
            assert!(
                (back - chosen).abs() < 1e-4,
                "toggle {i} came back to {back}, not to the reader's {chosen}"
            );
            assert!(
                (relaxed - settled[0].0).abs() < 1e-4,
                "toggle {i} relaxed to {relaxed}, not to {} as the first did",
                settled[0].0
            );
        }
    }

    #[test]
    fn settle_view_pans_an_offscreen_target_into_view_without_rezoom() {
        // A small rect off to the right fits at the current zoom, so the zoom
        // holds and only the pan shifts it back into frame.
        let target = Rect {
            x: 1000.0,
            y: 100.0,
            w: 50.0,
            h: 40.0,
        };
        let (z, p) = settle_view(target, vp(800., 600.), CONTENT, 1.0, (0.0, 0.0), 1.0);
        assert!((z - 1.0).abs() < 1e-4, "zoom moved: {z}");
        assert!(p.0 < 0.0, "expected a leftward pan, got {p:?}");
        assert_enclosed(target, 800., 600., z, p);
    }

    /// The window in layout space that the culling tests measure against.
    const WINDOW: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 10.0,
        h: 10.0,
    };

    #[test]
    fn a_route_is_kept_only_where_it_runs_through_the_window() {
        // A long diagonal whose bounding box swallows the window, but which
        // passes well clear of it: the bounds alone would keep it.
        let across = [(0.0, 1000.0), (1000.0, 0.0)];
        assert!(WINDOW.overlaps(&Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 1000.0
        }));
        assert!(!WINDOW.crossed_by(&across));
        assert!(WINDOW.crossed_by(&[(-100.0, 5.0), (100.0, 5.0)]));
        // A route entirely inside, and one that only clips a corner leg.
        assert!(WINDOW.crossed_by(&[(2.0, 2.0), (8.0, 8.0)]));
        assert!(WINDOW.crossed_by(&[(-5.0, -5.0), (-5.0, 5.0), (5.0, 5.0)]));
        assert!(!WINDOW.crossed_by(&[(-5.0, -5.0), (-5.0, 50.0), (-1.0, 50.0)]));
    }

    #[test]
    fn a_straight_run_along_an_axis_still_registers() {
        // A vertical or horizontal leg spans no area, so an area test alone
        // would drop it.
        assert!(WINDOW.crossed_by(&[(5.0, -50.0), (5.0, 50.0)]));
        assert!(WINDOW.crossed_by(&[(-50.0, 5.0), (50.0, 5.0)]));
        assert!(!WINDOW.crossed_by(&[(50.0, -50.0), (50.0, 50.0)]));
    }

    #[test]
    fn ease_out_cubic_endpoints_and_deceleration() {
        assert!(ease_out_cubic(0.0).abs() < 1e-6, "{}", ease_out_cubic(0.0));
        assert!(
            (ease_out_cubic(1.0) - 1.0).abs() < 1e-6,
            "{}",
            ease_out_cubic(1.0)
        );
        // Ease-out: past the halfway mark in eased progress before halfway in time.
        assert!(ease_out_cubic(0.5) > 0.5, "{}", ease_out_cubic(0.5));
        // Monotonically increasing.
        let mut prev = ease_out_cubic(0.0);
        for i in 1..=10 {
            let e = ease_out_cubic(i as f32 / 10.0);
            assert!(e >= prev, "not monotonic at {i}: {e} < {prev}");
            prev = e;
        }
    }

    /// The slack `clamp_pan` allows past the flush position on an axis.
    fn clearance(vp_axis: f32, extent: f32) -> f32 {
        (1.0 - FIT_MARGIN) * vp_axis.min(extent)
    }

    #[test]
    fn clamp_pan_keeps_smaller_content_within_the_viewport() {
        // Content smaller than the viewport (zoom 0.05) stays inside, give or take the
        // edge clearance, so the pan stays in -clearance..=(viewport - content + clearance).
        let viewport = vp(800., 600.);
        let zoom = 0.05;
        let (cw, ch) = (CONTENT.0 * zoom, CONTENT.1 * zoom);
        let (mx, my) = (clearance(800., cw), clearance(600., ch));
        assert_eq!(
            clamp_pan((-9999.0, -9999.0), zoom, viewport, CONTENT),
            (-mx, -my)
        );
        assert_eq!(
            clamp_pan((9999.0, 9999.0), zoom, viewport, CONTENT),
            (800.0 - cw + mx, 600.0 - ch + my)
        );
        // A pan within the bound is left untouched, including one that overscrolls
        // into the clearance.
        assert_eq!(
            clamp_pan((-50.0, -40.0), zoom, viewport, CONTENT),
            (-50.0, -40.0)
        );
        assert_eq!(
            clamp_pan((100.0, 80.0), zoom, viewport, CONTENT),
            (100.0, 80.0)
        );
    }

    #[test]
    fn clamp_pan_keeps_larger_content_covering_the_viewport() {
        // Content larger than the viewport (zoom 1.0) may pull a clearance-sized gutter
        // past an edge but no further, so the pan stays in
        // (viewport - content - clearance)..=clearance.
        let viewport = vp(800., 600.);
        let (cw, ch) = (CONTENT.0, CONTENT.1);
        let (mx, my) = (clearance(800., cw), clearance(600., ch));
        assert_eq!(
            clamp_pan((9999.0, 9999.0), 1.0, viewport, CONTENT),
            (mx, my)
        );
        assert_eq!(
            clamp_pan((-9999.0, -9999.0), 1.0, viewport, CONTENT),
            (800.0 - cw - mx, 600.0 - ch - my)
        );
        // The flush-cover position sits inside the bound, so it is left alone.
        assert_eq!(clamp_pan((0.0, 0.0), 1.0, viewport, CONTENT), (0.0, 0.0));
    }

    #[test]
    fn clamp_pan_never_carries_a_small_graph_off_an_edge() {
        let viewport = vp(800., 600.);
        let content = (60.0, 40.0);
        let near = clamp_pan((-9999.0, -9999.0), 1.0, viewport, content);
        let far = clamp_pan((9999.0, 9999.0), 1.0, viewport, content);
        assert!(
            near.0 + content.0 > 0. && near.1 + content.1 > 0.,
            "off the near edge: {near:?}"
        );
        assert!(far.0 < 800. && far.1 < 600., "off the far edge: {far:?}");
    }

    /// Float slack in the anchor assertions, far below a pixel of visible drift.
    const ANCHOR_TOLERANCE: f32 = 0.05;

    /// A canvas laid out away from the window origin, so a step that neglects to
    /// take the canvas offset off the cursor lands visibly wrong.
    fn canvas(w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), vp(w, h))
    }

    /// Cursor positions over `bounds` covering its center, its four edges and its
    /// four corners.
    fn cursors(bounds: Bounds<Pixels>) -> impl Iterator<Item = Point<Pixels>> {
        const ALONG: [f32; 3] = [0.02, 0.5, 0.98];
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        ALONG.into_iter().flat_map(move |a| {
            ALONG
                .into_iter()
                .map(move |b| point(px(ox + a * w), px(oy + b * h)))
        })
    }

    /// How far the layout point under `cursor` travels on screen across one wheel
    /// step: the drift the cursor anchoring exists to hold at zero.
    fn anchor_drift(
        cursor: Point<Pixels>,
        camera: Camera,
        notches: f32,
        bounds: Bounds<Pixels>,
        content: (f32, f32),
    ) -> (f32, f32) {
        let (gx, gy) = from_screen(cursor, bounds.origin, camera);
        let after = zoom_step(cursor, camera, notches, bounds, content);
        let landed = to_screen(gx as f64, gy as f64, bounds, after);
        (
            f32::from(landed.x) - f32::from(cursor.x),
            f32::from(landed.y) - f32::from(cursor.y),
        )
    }

    /// Asserts a step in and a step out hold their anchor at every cursor over
    /// `bounds`, naming `state` in the failure so the camera it started from is
    /// identifiable.
    fn assert_anchored(state: &str, camera: Camera, bounds: Bounds<Pixels>, content: (f32, f32)) {
        for cursor in cursors(bounds) {
            for notches in [1.0, -1.0] {
                let (dx, dy) = anchor_drift(cursor, camera, notches, bounds, content);
                assert!(
                    dx.abs() < ANCHOR_TOLERANCE && dy.abs() < ANCHOR_TOLERANCE,
                    "{state}: {notches:+} notches at {cursor:?} slipped the anchor by ({dx}, {dy}) px"
                );
            }
        }
    }

    /// The camera showing `content` centered in `bounds` at `zoom`.
    fn centered(zoom: f32, bounds: Bounds<Pixels>, content: (f32, f32)) -> Camera {
        let (vw, vh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        Camera {
            zoom,
            pan: ((vw - content.0 * zoom) / 2., (vh - content.1 * zoom) / 2.),
        }
    }

    #[test]
    fn a_wheel_step_holds_its_anchor_while_the_graph_fits_the_window() {
        let bounds = canvas(800., 600.);
        let (zoom, pan) = fit_view(bounds.size, CONTENT);
        assert_anchored("fitted", Camera { zoom, pan }, bounds, CONTENT);
    }

    #[test]
    fn a_wheel_step_holds_its_anchor_when_one_axis_overflows() {
        // A wide graph zoomed until its width spills past the window while its
        // height still fits, so one scroll bar is up.
        const WIDE: (f32, f32) = (4000.0, 900.0);
        let bounds = canvas(800., 600.);
        let zoom = 0.4;
        assert!(WIDE.0 * zoom > 800. && WIDE.1 * zoom < 600.);
        assert_anchored("wide graph", centered(zoom, bounds, WIDE), bounds, WIDE);
    }

    #[test]
    fn a_wheel_step_holds_its_anchor_when_both_axes_overflow() {
        let bounds = canvas(800., 600.);
        let zoom = 0.5;
        assert!(CONTENT.0 * zoom > 800. && CONTENT.1 * zoom > 600.);
        let (cw, ch) = (CONTENT.0 * zoom, CONTENT.1 * zoom);
        for (state, pan) in [
            ("centered", ((800. - cw) / 2., (600. - ch) / 2.)),
            ("flush at the near edges", (0.0, 0.0)),
            ("flush at the far edges", (800. - cw, 600. - ch)),
            ("a gutter past the near edges", (100.0, 80.0)),
        ] {
            assert_anchored(state, Camera { zoom, pan }, bounds, CONTENT);
        }
    }

    #[test]
    fn every_step_of_a_wheel_run_holds_its_anchor() {
        // What a wheel actually sends: notch after notch at one cursor, from a
        // view scrolled off the near edges of a graph larger than the window.
        let bounds = canvas(800., 600.);
        let cursor = point(px(10. + 600.), px(20. + 450.));
        let mut camera = Camera {
            zoom: 0.5,
            pan: (100.0, 80.0),
        };
        for step in 0..4 {
            let (dx, dy) = anchor_drift(cursor, camera, -1.0, bounds, CONTENT);
            assert!(
                dx.abs() < ANCHOR_TOLERANCE && dy.abs() < ANCHOR_TOLERANCE,
                "step {step} slipped the anchor by ({dx}, {dy}) px"
            );
            camera = zoom_step(cursor, camera, -1.0, bounds, CONTENT);
        }
    }

    #[test]
    fn a_wheel_run_into_max_zoom_stops_there_holding_its_anchor() {
        let bounds = canvas(800., 600.);
        let camera = Camera {
            zoom: MAX_ZOOM * 0.98,
            pan: (-4000.0, -3000.0),
        };
        let cursor = point(px(10. + 500.), px(20. + 200.));
        assert_eq!(
            zoom_step(cursor, camera, 5.0, bounds, CONTENT).zoom,
            MAX_ZOOM
        );
        let (dx, dy) = anchor_drift(cursor, camera, 5.0, bounds, CONTENT);
        assert!(
            dx.abs() < ANCHOR_TOLERANCE && dy.abs() < ANCHOR_TOLERANCE,
            "({dx}, {dy}) px"
        );
    }

    #[test]
    fn a_wheel_run_pinned_in_a_corner_keeps_the_graph_in_view() {
        // A cursor in the corner asks the content to shrink away into it, which the
        // pan bound refuses past its clearance: the anchor gives before the graph
        // leaves the window.
        let bounds = canvas(800., 600.);
        let cursor = point(px(10. + 784.), px(20. + 588.));
        let mut camera = Camera {
            zoom: 0.5,
            pan: (0.0, 0.0),
        };
        for _ in 0..20 {
            camera = zoom_step(cursor, camera, -1.0, bounds, CONTENT);
            let (w, h) = (CONTENT.0 * camera.zoom, CONTENT.1 * camera.zoom);
            let on_screen = |pan: f32, extent: f32, vp: f32| (pan + extent).min(vp) - pan.max(0.);
            let (sx, sy) = (
                on_screen(camera.pan.0, w, 800.),
                on_screen(camera.pan.1, h, 600.),
            );
            let eps = 1e-2;
            assert!(
                sx + eps >= FIT_MARGIN * w.min(800.) && sy + eps >= FIT_MARGIN * h.min(600.),
                "only ({sx}, {sy}) px of graph left on screen at zoom {}",
                camera.zoom
            );
        }
    }

    #[test]
    fn a_wheel_run_into_the_fit_zoom_stops_there_holding_its_anchor() {
        let bounds = canvas(800., 600.);
        let fit = fit_zoom(bounds.size, CONTENT);
        let camera = centered(fit * 1.05, bounds, CONTENT);
        let cursor = point(px(10. + 400.), px(20. + 300.));
        assert_eq!(zoom_step(cursor, camera, -5.0, bounds, CONTENT).zoom, fit);
        let (dx, dy) = anchor_drift(cursor, camera, -5.0, bounds, CONTENT);
        assert!(
            dx.abs() < ANCHOR_TOLERANCE && dy.abs() < ANCHOR_TOLERANCE,
            "({dx}, {dy}) px"
        );
    }

    fn source(paths: &[&str]) -> GraphSource {
        let inputs = paths
            .iter()
            .map(|path| bviz_ui_graph_args::Input {
                kind: bviz_ui_graph_args::InputKind::Aquery,
                path: path.into(),
            })
            .collect();
        GraphSource::new(inputs).expect("the paths name a graph")
    }

    /// One graph is drawn however many files it merges, so its title names the
    /// first and counts the rest.
    #[test]
    fn a_graph_of_several_files_is_titled_by_the_first_and_a_count() {
        assert_eq!(title(&source(&["/tmp/aquery.pb"])), "aquery.pb");
        assert_eq!(
            title(&source(&["/tmp/aquery.pb", "/tmp/cquery.pb"])),
            "aquery.pb +1"
        );
        assert_eq!(
            title(&source(&[
                "/tmp/aquery.pb",
                "/tmp/cquery.pb",
                "/tmp/query.pb"
            ])),
            "aquery.pb +2"
        );
    }
}
