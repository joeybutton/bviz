use crate::camera::{Camera, Rect, to_screen, visible_rect};
use crate::colour::Colouring;
use crate::critical::CriticalNodes;
use crate::derive::{self, Drawn, Expandable};
use crate::morph::Frame;
use crate::neighborhood::{Neighborhood, compute_neighborhood};
use crate::pan_bench::{self, Phase, Tally};
use crate::playback::{NodeState, NodeStates, Running, Stage};
use crate::relation::{Direction, Relation};
use crate::scroll::paint_scrollbars;
use crate::status::{Status, Statuses};
use crate::timing::{NodeTimes, Ran};
use crate::{GraphPanel, Palette};
use bviz_graph_layout::layout::{CARET_RESERVE, LABEL_BAND};
use bviz_graph_layout::transition::Transition;
use bviz_graph_layout::{Connector, EdgeKind, Id, Kind};
use bviz_invocation::{Done, EpochMicros, Runner, Venue};
use gpui::{
    App, Background, BorderStyle, Bounds, Entity, Hsla, Path, PathBuilder, Pixels, Point, Rgba,
    TextAlign, Window, point, px, quad, size,
};
use rustc_hash::FxHashMap;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

/// Box label font size in layout pixels.
const LABEL_SIZE: f64 = 13.0;

/// Screen-pixel size a label is never drawn below. Zoom scales the label with
/// the box down to this floor and no further, so a box wide enough to hold its
/// name shows it rather than fading to a blank rectangle.
const LABEL_FLOOR: f32 = 11.0;

/// Screen-pixel clearance held between a label and its box's edges, so a label
/// held at the floor is not painted flush against the border.
const LABEL_PAD: f32 = 2.0;

/// Screen width a box needs to spare for its label before one is drawn: any
/// narrower and what survives truncation names nothing, so the box is left bare.
const LABEL_MIN_WIDTH: f32 = 22.0;

/// Arrowhead size along and across the edge direction, in layout pixels.
const ARROW_LENGTH: f64 = 9.0;
const ARROW_HALF_WIDTH: f64 = 4.0;
/// How far the stroke runs past the arrowhead base into the head, closing the
/// seam without letting the line broaden the visible tip.
const ARROW_OVERDRAW: f64 = 1.0;

/// Stroke width (pre-zoom px) of the line a connector's provenance is drawn in.
const CONNECTOR_WIDTH: f32 = 1.5;

/// Screen width the thinnest connector line is never drawn below. A band
/// narrower than a pixel can pass between the rasterizer's samples entirely, so
/// whether the line is drawn at all turns on where the pan happens to leave it;
/// a pixel across, it meets a sample wherever it falls.
const CONNECTOR_FLOOR: f32 = 1.0;

/// The zoom at which the thinnest line reaches [`CONNECTOR_FLOOR`]: at and above
/// it the camera sets every stroke's width, below it the floor does.
const FLOORED_BELOW: f32 = CONNECTOR_FLOOR / CONNECTOR_WIDTH;

/// The share of its own opacity a connector is never drawn below, however far
/// the zoom is out: a route a map draws too faint to find is off the map. Set
/// where a line thinned to that share still stands a
/// [`bviz_ui_theme::MIN_DISTINCT_COLOR`] off every ground it crosses, which is
/// the distance the unthinned line is held to.
const TAPER_FLOOR: f32 = 0.3;

/// Length (pre-zoom px) of one dash of a bridged route and of the gap after it,
/// the gap wide enough that the dashes read as separate marks at the widths a
/// bundle of lanes can reach.
const BRIDGE_DASH: f32 = 7.0;
const BRIDGE_GAP: f32 = 5.0;

/// Screen length a dash or a gap of a bridged route is never drawn below. Path
/// coverage is sampled rather than computed, so a pattern the camera has shrunk
/// toward a pixel is drawn or dropped by where the pan happens to leave each
/// dash, and the route flickers between a dotted line and a solid one; several
/// pixels long, every dash and every gap meets samples wherever it falls.
const BRIDGE_PATTERN_FLOOR: f32 = 3.0;

/// How far a bridged route's ink is carried toward the canvas in perceptual
/// lightness. A bridge stands for a path through boxes the drawing does not
/// hold, which is a weaker claim than a dependency the graph states, and the
/// zoom that floors the dashes goes on to close them up into a solid line — so
/// the distinction is carried in lightness as well, which no zoom takes away.
/// Bounded above by the dimmed line, thinned by the taper, holding
/// [`bviz_ui_theme::MIN_DISTINCT_COLOR`] off every ground it crosses and every
/// ink it runs beside, which is the distance every route is held to.
const BRIDGE_DIM: f32 = 0.035;

/// Stroke width (pre-zoom px) of that line where a marking picks the connector out.
/// A doubling is the smallest step that reads as a heavier line rather than as
/// an artifact of antialiasing.
const MARKED_WIDTH: f32 = 2.0 * CONNECTOR_WIDTH;

/// How far a mark carries a connector's color in perceptual lightness,
/// away from the canvas. Bounded below by a marked line standing
/// [`bviz_ui_theme::MIN_DISTINCT_COLOR`] off the unmarked line of its own query,
/// and above by the three provenances holding that distance from one another
/// however each of a pair is marked, and from the inks a route is drawn beside.
/// Set where the nearest of those bounds is furthest off.
const MARK_LIFT: f32 = 0.072;

/// How far a split's badge is carried from the purple slot toward the red one:
/// halfway, to a magenta no slot holds and neither neighbour's stroke wears.
const SPLIT_BLEND: f32 = 0.5;

/// Half the diagonal (pre-zoom px) of the diamond set halfway along a connector
/// that crosses a configuration transition, wide enough to stand across every
/// lane of a bundle so the mark reads as the pair's rather than one lane's.
const BADGE_HALF: f32 = 5.0;

/// Screen half-diagonal a transition badge is never drawn below: it marks an
/// edge a reader is looking for, which zooming out to find must not shrink away.
pub(crate) const BADGE_FLOOR: f32 = 4.0;

/// Screen width of the canvas-coloured rim around a transition badge, which
/// parts the badge from the route it sits on.
const BADGE_RIM: f32 = 1.5;

/// Half the width (pre-zoom px) of the glyph a status badge is drawn as, which
/// stands it as tall as the capitals of the label it is set before.
const STATUS_HALF: f32 = 5.0;

/// Screen half-width a status glyph is never drawn below. Its shape is what
/// tells one status from another, and fewer pixels than this leave every shape
/// a dot.
pub(crate) const STATUS_FLOOR: f32 = 4.0;

/// Screen width of the canvas-coloured rim around a status glyph. A status
/// colour is held to its contrast against the canvas, which no fill a box wears
/// answers for, so the glyph brings that ground with it.
pub(crate) const STATUS_RIM: f32 = 1.5;

/// Bare ground (pre-zoom px) between a status badge and what follows it in a
/// label.
const STATUS_GAP: f32 = 2.0;

/// The share of a status glyph's half-width a ring's stroke takes, and the
/// screen width that stroke is never drawn below.
const RING_STROKE: f32 = 0.45;
const RING_STROKE_FLOOR: f32 = 1.5;

/// The share of a status glyph's half-width a bar's half-height takes.
const BAR_HALF_HEIGHT: f32 = 0.4;

/// The share of a status glyph's half-width a square's half-side takes, set
/// below the whole so the square weighs no more than the disc.
const SQUARE_HALF_SIDE: f32 = 0.85;

/// The shares of a status glyph's half-width a triangle's apex stands above
/// its middle and its base lies below it: short of the whole, so the triangle
/// weighs no more than the disc.
const TRIANGLE_APEX: f32 = 0.95;
const TRIANGLE_BASE: f32 = 0.8;

/// The share of a status glyph's half-width a cross's arms are thick to
/// either side of their middle.
const CROSS_ARM: f32 = 0.3;

/// Bare ground (pre-zoom px) left between the strokes of two adjacent lanes, so
/// a bundle of them reads as several lines rather than as one broad one. The
/// zoom carries it along with the strokes it separates, so what holds at every
/// zoom is its ratio to them.
const LANE_GAP: f32 = 1.0;

/// Perpendicular gap (pre-zoom px) between the parallel provenance lines drawn
/// along a node pair that several queries connect. Either lane may be marked and
/// so drawn at [`MARKED_WIDTH`], and every lane of an unmarked bundle pays for
/// that too.
const PARALLEL_SPACING: f32 = MARKED_WIDTH + LANE_GAP;

/// How far a kind accent is carried toward the canvas to make a leaf fill.
/// Bounded below by the fills staying clear of the connector colors drawn over
/// and between them, which are taken from the same slots, and above by the kinds
/// staying apart from one another and off the canvas.
const KIND_BLEND: f32 = 0.6;

/// How far the canvas is carried toward the yellow slot to fill the box
/// holding the least time of its kind.
const RAMP_LEAST: f32 = 0.225;

/// How far the canvas is carried toward the blue slot to fill the box holding
/// the most time of its kind.
const RAMP_MOST: f32 = 0.45;

/// How far an open container's fill is tinted toward the reading it wears
/// closed.
const OPEN_TINT: f32 = 0.5;

/// How far a runner's fill is carried toward the canvas in perceptual
/// lightness, past the blend toward it, which parts the warm slots that
/// otherwise crowd one another.
const RUNNER_DIM: f32 = 0.08;

const REMOTE_EXECUTION_BLEND: f32 = 0.575;
const WORKER_BLEND: f32 = 0.425;
const SANDBOXED_BLEND: f32 = 0.65;
const LOCAL_BLEND: f32 = 0.45;
const REMOTE_CACHE_HIT_BLEND: f32 = 0.375;
const DISK_CACHE_HIT_BLEND: f32 = 0.575;
const NO_SPAWN_RECORDED_BLEND: f32 = 0.525;

/// How a playback state's fill is made from its slot: carried `blend` toward
/// the canvas, then `lift` away from it in perceptual lightness, toward it
/// where negative. Each state's pair is as far as sets it apart from the other
/// states within the bounds every fill is held to.
#[derive(Clone, Copy)]
struct StateTint {
    blend: f32,
    lift: f32,
}

const PENDING_TINT: StateTint = StateTint {
    blend: 0.445,
    lift: 0.015,
};
const PREPARING_TINT: StateTint = StateTint {
    blend: 0.65,
    lift: -0.06,
};
const CHECKING_CACHE_TINT: StateTint = StateTint {
    blend: 0.633,
    lift: -0.018,
};
const SCHEDULING_TINT: StateTint = StateTint {
    blend: 0.59,
    lift: 0.008,
};
const RUNNING_LOCALLY_TINT: StateTint = StateTint {
    blend: 0.2625,
    lift: -0.18,
};
const RUNNING_TINT: StateTint = StateTint {
    blend: 0.0125,
    lift: -0.28,
};
const RUNNING_REMOTELY_TINT: StateTint = StateTint {
    blend: 0.2625,
    lift: -0.2,
};
const EXECUTED_TINT: StateTint = StateTint {
    blend: 0.632,
    lift: 0.0,
};
const CACHED_TINT: StateTint = StateTint {
    blend: 0.441,
    lift: -0.146,
};

/// How far a playback state's slot is carried from the first of the two slots
/// it lies between toward the second.
const PREPARING_HUE: f32 = 0.88;
const RUNNING_HUE: f32 = 0.4;
const RUNNING_REMOTELY_HUE: f32 = 0.05;
const CACHED_HUE: f32 = 0.26;

/// How far the neutral slot is carried toward the canvas to fill a box whose
/// actions ran on a mix of runners.
const MIXED_BLEND: f32 = 0.4;

/// How far the mixed fill is then carried away from the canvas in perceptual
/// lightness.
const MIXED_LIFT: f32 = 0.04;

/// How far a mark standing for a mix of runners is carried from the neutral
/// slot toward the label ink, away from the canvas.
const MIXED_MARK_TINT: f32 = 0.5;

/// How far a group's fill is tinted toward the accent of the files it holds: far
/// enough to be told from a plain container fill, and not so far that the group
/// passes for one of the files.
const GROUP_TINT: f32 = 0.17;

/// Opacity of the wash a marked box's fill is painted over. Bounded below by the
/// wash carrying the fill a [`bviz_ui_theme::MIN_DISTINCT_COLOR`] off the one the
/// box wears unmarked, so the mark is seen at all, and above by the box's name —
/// inked from the unwashed fill — holding [`bviz_ui_theme::MIN_TEXT_CONTRAST`] on
/// the ground the wash makes of it.
const HIGHLIGHT_WASH: f32 = 0.155;

/// The share of its own opacity a box or connector is drawn at where a standing
/// selection leaves it unmarked, so what the selection picks out reads against a
/// graph that has receded rather than against one drawn just as strongly. Low
/// enough that the marked part carries the eye on its own, and above zero so the
/// rest still says where in the graph that part sits.
pub(crate) const UNMARKED_FADE: f32 = 0.2;

/// Width of the halo around a box standing for a step of the critical path,
/// in layout pixels: wide enough to read as a mark of its own beside the
/// accent border a selection draws.
const CRITICAL_HALO: f32 = 3.0;

/// The narrowest the critical-path halo is drawn on screen, in pixels: it
/// lies outside the box, so a box zoomed out to a few pixels is found by the
/// halo's size rather than its own.
const CRITICAL_HALO_FLOOR: f32 = 4.0;

/// Radius of a box's corners, in layout pixels.
const BOX_CORNER: f32 = 6.0;

/// What a frame of the canvas draws from that is settled as its scene is
/// built, owned so the `'static` paint closure holds no borrow of the view.
pub(crate) struct Scene {
    pub(crate) drawn: Drawn,
    pub(crate) expandable: Expandable,
    pub(crate) palette: Palette,
    /// The boxes the filter draws as no part of what it selects, which it fades
    /// rather than leaving out so the reader sees what hiding them would do.
    pub(crate) faded: Arc<HashSet<Id>>,
    /// The box the pointer rests on, whose direct neighbors it marks for as long
    /// as it does; `None` when the pointer is off the boxes or the marking is
    /// switched off.
    pub(crate) hovered: Option<Id>,
    /// The selected box, which wears the accent border; what it marks besides is
    /// the selection's to say.
    pub(crate) selected: Option<Id>,
    /// The emphasized box and the alpha its highlight paints at.
    pub(crate) highlight: Option<(Id, f32)>,
    pub(crate) colouring: Colouring,
    /// The document's playback time, which a colouring by playback reads;
    /// `None` where the document does not play back.
    pub(crate) at: Option<EpochMicros>,
    /// Whether the boxes standing for the critical path's steps wear its halo.
    pub(crate) critical_path: bool,
}

impl Scene {
    /// The share of its own opacity the filter leaves whatever `id` names: all
    /// of it, and [`UNMARKED_FADE`] of it where the filter draws `id` as no part
    /// of what it selects.
    fn filter_strength(&self, id: Id) -> f32 {
        match self.faded.contains(&id) {
            true => UNMARKED_FADE,
            false => 1.0,
        }
    }

    /// What a connector between `from` and `to` is left at by the filter, which
    /// fades a route into or out of a box it fades along with the box.
    fn edge_strength(&self, from: Id, to: Id) -> f32 {
        self.filter_strength(from).min(self.filter_strength(to))
    }
}

/// Everything a frame draws marked. A box or a connector either marking picks
/// out is drawn marked, so a selection holds its marks under a hover passing
/// over another part of the graph.
struct Marks {
    hover: Neighborhood,
    selection: Rc<Neighborhood>,
}

impl Marks {
    fn marks_box(&self, id: Id) -> bool {
        self.hover.boxes.contains(&id) || self.selection.boxes.contains(&id)
    }

    fn marks_edge(&self, edge: (Id, Id)) -> bool {
        self.hover.edges.contains(&edge) || self.selection.edges.contains(&edge)
    }

    /// The share of its own opacity something standing at `marked` is drawn at:
    /// all of it wherever the selection marks nothing in this drawing — because
    /// none is made, or because the view folds the selected box away — and
    /// [`UNMARKED_FADE`] of it for whatever a selection that does mark something
    /// here leaves out.
    fn strength(&self, marked: bool) -> f32 {
        match marked || self.selection.boxes.is_empty() {
            true => 1.0,
            false => UNMARKED_FADE,
        }
    }

    /// What a connector standing at `marked` and left at `filtered` by the
    /// filter is drawn at, `edges` being the share a morph in flight leaves its
    /// routes at. A mark buys no exemption from [`ink_taper`]: how much of a
    /// drawn band is the line's own is the camera's to answer, and a marked line
    /// meets the camera on the same terms as the rest, so a selection stands its
    /// routes off the graph by as much at every zoom.
    fn connector_alpha(&self, marked: bool, filtered: f32, edges: f32, zoom: f32) -> f32 {
        self.badge_alpha(marked, filtered, edges) * ink_taper(zoom)
    }

    /// What a mark set on a connector standing at `marked`, `filtered` and
    /// `edges` is drawn at. A badge is held to a screen size, so it keeps its
    /// ink at every zoom.
    fn badge_alpha(&self, marked: bool, filtered: f32, edges: f32) -> f32 {
        edges * receded(self.strength(marked), filtered)
    }
}

/// The share of its own opacity something the selection leaves at `marked` and
/// the filter at `filtered` is drawn at. Both say the same thing — this is no
/// part of what the reader picked out — so the fainter of the two decides;
/// multiplied together they would take a box both leave out past the point of
/// being seen at all.
fn receded(marked: f32, filtered: f32) -> f32 {
    marked.min(filtered)
}

/// The canvas paint pass: box fills parent-before-child so children sit over
/// their containers, then the connectors, then every label on top.
///
/// Labels go last, in a pass of their own, so a name wins over the routes
/// crossing its box; at the finest levels the lanes are dense enough to shred
/// one painted under them. That pass keeps the boxes' outermost-first order,
/// which is the order [`paint_box_label`] arbitrates overlapping titles in.
///
/// A morph in flight places each box between the drawing it came from and the
/// one it is going to. The boxes only the departing drawing held come after the
/// rest, so a member fading into the container it collapses into is drawn over
/// it rather than under.
///
/// The camera is read here, once the prepaint has fitted it, so the level a map
/// draws and the part of it the window shows are both the frame's own. Only what
/// falls inside that part is touched, which is what keeps a level of tens of
/// thousands of boxes paintable.
pub(crate) fn canvas_body(
    scene: Scene,
    entity: Entity<GraphPanel>,
) -> impl FnOnce(Bounds<Pixels>, (), &mut Window, &mut App) {
    move |bounds, _, window, cx| {
        let _span = pan_bench::span(Phase::CanvasPaint);
        let (camera, morph) = {
            let view = entity.read(cx);
            (view.camera(), view.morph_frame())
        };
        entity.update(cx, |view, cx| {
            derive::derive_view(view, &scene.drawn, camera.zoom, cx);
        });
        let Some(g) = scene.drawn.view(camera.zoom) else {
            return;
        };
        let visible = visible_rect(bounds, camera);
        // A hover is gone the moment the pointer moves, so its marking is derived
        // afresh; a selection outlives the frames that draw it, so it holds its
        // own and hands out a share in it.
        let marks = Marks {
            hover: compute_neighborhood(g, scene.hovered, Relation::Direct(Direction::Both)),
            selection: entity.update(cx, |view, _| {
                view.selection.marking(g, scene.drawn.view_key(camera.zoom))
            }),
        };
        let Some(loaded) = entity.read(cx).model.read(cx).loaded() else {
            return;
        };
        let statuses = loaded.statuses();
        let fills = Fills::of(scene.colouring, loaded.times().ok(), loaded.states());
        let critical = scene.critical_path.then(|| loaded.critical());
        let drawn: Vec<(PaintBox, Bounds<Pixels>)> = g
            .boxes
            .iter()
            .map(|b| {
                let (rect, alpha) = morph.as_ref().map_or((Rect::of(b), 1.0), |m| m.place(b));
                Placed {
                    b,
                    rect,
                    alpha,
                    leaving: false,
                }
            })
            .chain(
                morph
                    .iter()
                    .flat_map(Frame::leaving)
                    .map(|(b, rect, alpha)| Placed {
                        b,
                        rect,
                        alpha,
                        leaving: true,
                    }),
            )
            .filter(|p| visible.overlaps(&p.rect))
            .map(|p| {
                let b = PaintBox::of(&p, &scene, &marks, statuses, &fills, critical);
                let rect = screen_rect(&b, bounds, camera);
                paint_box(&b, rect, camera.zoom, &scene.palette, window);
                (b, rect)
            })
            .collect();
        pan_bench::tally(Tally::Boxes, drawn.len());
        let edges = morph.as_ref().map_or(1.0, |m| m.connectors);
        let mut connectors = 0;
        // Painted over the labels, which a badge at a route's middle can fall
        // under where the route crosses a container's title band.
        let mut badges: Vec<(&Connector, Transition, f32)> = Vec::new();
        if edges > 0.0 {
            order_fence(bounds, window);
            let on_screen: HashSet<Id> = drawn.iter().map(|(b, _)| b.id).collect();
            for c in &g.connectors {
                if drawn_route(c, &visible, &on_screen) {
                    let marked = marks.marks_edge((c.from, c.to));
                    let filtered = scene.edge_strength(c.from, c.to);
                    let alpha = marks.connector_alpha(marked, filtered, edges, camera.zoom);
                    paint_lanes(c, marked, alpha, bounds, camera, &scene.palette, window);
                    connectors += 1;
                    if let Some(transition) = c.transition {
                        badges.push((c, transition, marks.badge_alpha(marked, filtered, edges)));
                    }
                }
            }
            order_fence(bounds, window);
        }
        pan_bench::tally(Tally::Connectors, connectors);
        for (b, rect) in drawn.iter().filter(|(b, _)| b.critical) {
            paint_critical_halo(b, *rect, camera.zoom, &scene.palette, window);
        }
        let mut floated: Vec<Bounds<Pixels>> = Vec::new();
        let mut titles: Vec<TitleTarget> = Vec::new();
        // Painted once every label is, so that no plate lands on a badge.
        let mut statuses: Vec<StatusBadge> = Vec::with_capacity(drawn.len());
        let mut bare: Vec<BareBadge> = Vec::with_capacity(drawn.len());
        for (b, rect) in &drawn {
            let label = paint_box_label(
                b,
                *rect,
                camera.zoom,
                &scene.palette,
                &mut floated,
                window,
                cx,
            );
            match label {
                Some(label) => {
                    titles.extend(label.title);
                    statuses.extend(label.badge);
                }
                None => bare.extend(b.bare_badge(*rect, camera.zoom)),
            }
        }
        place_bare(bare, &floated, &mut statuses);
        entity.update(cx, |view, _| view.titles = titles);
        for (c, transition, alpha) in badges {
            paint_badge(c, transition, alpha, bounds, camera, &scene.palette, window);
        }
        for badge in statuses {
            badge.paint(&scene.palette, window);
        }
        paint_scrollbars(
            bounds,
            scene.drawn.content(),
            camera,
            &scene.palette,
            window,
        );
    }
}

/// Draws everything painted after it within `bounds` over everything painted
/// before it. gpui's wgpu renderer draws a run of paths as one batch, a single
/// offscreen multisampled pass, only while no other primitive's draw order
/// falls among theirs, and a primitive otherwise takes its order from whatever
/// it overlaps; fencing a run of paths on both sides keeps it one batch however
/// its members interleave on screen with the primitives around them.
fn order_fence(bounds: Bounds<Pixels>, window: &mut Window) {
    window.paint_layer(bounds, |_| {});
}

/// Whether a frame draws the route `c`: it crosses the view, and either the
/// view could hold the whole of it or `on_screen` — the boxes this frame drew —
/// holds a box at one of its ends. A route anchored to a box in sight is worth
/// following wherever its far end lies, and how far it reaches says nothing
/// about that: a connector meets its boxes at their borders, and a box wider
/// than the view is ordinary at a deep zoom. Left out is the route with neither
/// end in sight, which only crosses the view on its way between two places the
/// reader cannot see.
///
/// What a marking picks out has no bearing on it. A mark reaches routes whose
/// boxes are both off screen, and exempting those would lay back over the view
/// exactly the grid the reach test is here to take off it.
fn drawn_route(c: &Connector, visible: &Rect, on_screen: &HashSet<Id>) -> bool {
    visible.crossed_by(&c.points)
        && (visible.could_hold(&c.points)
            || on_screen.contains(&c.from)
            || on_screen.contains(&c.to))
}

/// `c` at `alpha` of its own opacity, by which a box takes its whole appearance
/// — fill, border, label — down together.
fn faded(c: Rgba, alpha: f32) -> Rgba {
    Rgba {
        a: c.a * alpha,
        ..c
    }
}

impl Palette {
    /// Accent for a leaf box, keyed by its node kind. Actions, artifacts and
    /// source files each take a slot of their own; every other kind shares the
    /// catch-all.
    pub(crate) fn kind_accent(&self, kind: Kind) -> Rgba {
        match kind {
            Kind::Action => self.data.orange,
            Kind::Artifact => self.data.green,
            Kind::Source => self.data.purple,
            _ => self.data.blue,
        }
        .into()
    }

    /// Fill for a leaf box, its kind's accent carried toward the canvas so the
    /// box reads as ground a name is set on rather than as a mark.
    fn leaf_fill(&self, kind: Kind) -> Rgba {
        bviz_ui_theme::mix(self.kind_accent(kind), self.background, KIND_BLEND)
    }

    /// Fill for a container box, by which nesting reads package > target >
    /// configured-target, and a group of files carries the accent of the files
    /// it holds.
    fn container_fill(&self, kind: Kind) -> Rgba {
        match kind {
            Kind::Package => self.package,
            Kind::MergeGroup => {
                bviz_ui_theme::mix(self.container, self.kind_accent(Kind::Artifact), GROUP_TINT)
            }
            _ => self.container,
        }
    }

    /// Fill for a box by the kind of node it stands for.
    fn kind_fill(&self, kind: Kind, compound: bool) -> Rgba {
        match compound {
            true => self.container_fill(kind),
            false => self.leaf_fill(kind),
        }
    }

    /// Fill for a box holding time, `place` being where that time stands from
    /// 0 for the least its kind holds to 1 for the most: a ramp from a pale
    /// yellow to a deeper blue, which stands further off the canvas the more
    /// time it says. Within the bounds every fill is held to — a name legible
    /// on it, the connectors and the marks drawn over it clear of it — and the
    /// least-timed box apart from an untimed one, its ends stand as far apart
    /// as the bounds let them.
    pub(crate) fn time_fill(&self, place: f32) -> Rgba {
        let least = bviz_ui_theme::mix(self.background, self.data.yellow, RAMP_LEAST);
        let most = bviz_ui_theme::mix(self.background, self.data.blue, RAMP_MOST);
        bviz_ui_theme::mix(least, most, place)
    }

    /// `fill` as a box wears it: whole on a box drawn closed, and on an open
    /// container only as a tint of the container's own fill, far enough that
    /// its reading shows and short of the whole: an open container is the
    /// ground the boxes it holds are drawn on, and one of those wearing the
    /// same reading in full must still stand off it.
    fn held_fill(&self, kind: Kind, compound: bool, fill: Rgba) -> Rgba {
        match compound {
            true => bviz_ui_theme::mix(self.container_fill(kind), fill, OPEN_TINT),
            false => fill,
        }
    }

    /// Fill for a box whose actions ran as `ran` says: its
    /// [slot](Self::ran_slot) carried toward the canvas, and for a mix of
    /// runners then away from it, so the mix reads as a fill of its own beside
    /// an untimed box and beside every runner.
    pub(crate) fn ran_fill(&self, ran: Ran) -> Rgba {
        let Ran::On(runner) = ran else {
            let mixed = bviz_ui_theme::mix(self.ran_slot(Some(ran)), self.background, MIXED_BLEND);
            return bviz_ui_theme::lighten(mixed, MIXED_LIFT.copysign(self.mark_lift()));
        };
        let (slot, blend) = self.runner_accent(runner);
        let carried = bviz_ui_theme::mix(slot, self.background, blend);
        bviz_ui_theme::lighten(carried, -RUNNER_DIM.copysign(self.mark_lift()))
    }

    /// The colour a point whose actions ran as `ran`, `None` where no time
    /// lands on it, is drawn in where it is a mark rather than ground: its
    /// slot, and for a mix of runners that slot carried away from the canvas,
    /// as a mixed box's fill is, so it stands apart from an untimed point.
    pub(crate) fn ran_mark(&self, ran: Option<Ran>) -> Rgba {
        let slot = self.ran_slot(ran);
        match ran {
            Some(Ran::Mixed) => bviz_ui_theme::mix(slot, self.label, MIXED_MARK_TINT),
            Some(Ran::On(_)) | None => slot.into(),
        }
    }

    /// The slot the actions of a box that ran as `ran`, `None` where no time
    /// lands on it, are coloured from: [each runner's](Self::runner_accent),
    /// and for a mix of runners or no time at all, which no runner stands for,
    /// the neutral slot.
    fn ran_slot(&self, ran: Option<Ran>) -> Hsla {
        match ran {
            Some(Ran::On(runner)) => self.runner_accent(runner).0,
            Some(Ran::Mixed) | None => self.data.neutral,
        }
    }

    /// The slot each runner is coloured from, every runner its own, and how
    /// far that slot is carried toward the canvas to fill a box. The slots
    /// crowd one another unevenly around the wheel, so each is carried as far
    /// as sets it apart from its neighbours within the bounds every fill is
    /// held to.
    fn runner_accent(&self, runner: Runner) -> (Hsla, f32) {
        match runner {
            Runner::RemoteExecution => (self.data.blue, REMOTE_EXECUTION_BLEND),
            Runner::Worker => (self.data.purple, WORKER_BLEND),
            Runner::Sandboxed => (self.data.orange, SANDBOXED_BLEND),
            Runner::Local => (self.data.red, LOCAL_BLEND),
            Runner::RemoteCacheHit => (self.data.cyan, REMOTE_CACHE_HIT_BLEND),
            Runner::DiskCacheHit => (self.data.green, DISK_CACHE_HIT_BLEND),
            Runner::NoSpawnRecorded => (self.data.yellow, NO_SPAWN_RECORDED_BLEND),
        }
    }

    /// Fill for a box standing at `state`. A box yet to start keeps to the
    /// neutral slot; one on its way to running deepens through blues, one
    /// running warms to the amber of a construction site (an action locally
    /// toward orange and remotely toward yellow, a container between them),
    /// and one done settles to a green low in chroma, a cache hit nearer the
    /// canvas and toward cyan.
    pub(crate) fn state_fill(&self, state: NodeState) -> Rgba {
        let d = &self.data;
        let between = bviz_ui_theme::mix;
        let (slot, tint) = match state {
            NodeState::Pending => (d.neutral.into(), PENDING_TINT),
            NodeState::In(Stage::Preparing) => {
                (between(d.cyan, d.blue, PREPARING_HUE), PREPARING_TINT)
            }
            NodeState::In(Stage::CheckingCache) => (d.blue.into(), CHECKING_CACHE_TINT),
            NodeState::In(Stage::Scheduling) => (d.blue.into(), SCHEDULING_TINT),
            NodeState::In(Stage::Running(Running::Container)) => {
                (between(d.orange, d.yellow, RUNNING_HUE), RUNNING_TINT)
            }
            NodeState::In(Stage::Running(Running::Action(Venue::Local))) => {
                (d.orange.into(), RUNNING_LOCALLY_TINT)
            }
            NodeState::In(Stage::Running(Running::Action(Venue::Remote))) => (
                between(d.yellow, d.orange, RUNNING_REMOTELY_HUE),
                RUNNING_REMOTELY_TINT,
            ),
            NodeState::Done(Done::Executed) => (d.green.into(), EXECUTED_TINT),
            NodeState::Done(Done::Cached) => (between(d.green, d.cyan, CACHED_HUE), CACHED_TINT),
        };
        let carried = bviz_ui_theme::mix(slot, self.background, tint.blend);
        bviz_ui_theme::lighten(carried, tint.lift * self.mark_lift().signum())
    }

    /// Fill for a box no time lands on: a container keeps the fill it wears
    /// by kind, and a leaf takes the [slot](Self::ran_slot) of no time at all.
    pub(crate) fn untimed_fill(&self, kind: Kind, compound: bool) -> Rgba {
        match compound {
            true => self.container_fill(kind),
            false => bviz_ui_theme::mix(self.ran_slot(None), self.background, KIND_BLEND),
        }
    }

    /// Connector color, keyed by the query an edge came from, so static (query),
    /// configured (cquery), and execution-phase (aquery) dependencies read
    /// distinctly; a filter's synthetic transitive edges take a fourth hue of
    /// their own. `marked` carries the color by [`MARK_LIFT`], which leaves
    /// untouched the hue that names the kind.
    pub(crate) fn edge_color(&self, kind: EdgeKind, marked: bool) -> Rgba {
        let slot = match kind {
            EdgeKind::Query => self.data.blue,
            EdgeKind::Cquery => self.data.cyan,
            EdgeKind::Aquery => self.data.orange,
            EdgeKind::Transitive => self.data.red,
        };
        let color = match marked {
            false => slot.into(),
            true => bviz_ui_theme::lighten(slot, self.mark_lift()),
        };
        match kind {
            EdgeKind::Transitive => bviz_ui_theme::lighten(color, self.bridge_dim()),
            _ => color,
        }
    }

    /// The fill of a transition badge: each kind its own, and no badge sharing
    /// a hue with an edge kind's stroke, so a badge never reads as a piece of
    /// route. The slots no edge kind takes are too few for every kind, so one
    /// kind takes a hue between two slots, and an unknown change, which says
    /// least of what changed, wears the label ink.
    pub(crate) fn transition_color(&self, transition: Transition) -> Rgba {
        match transition {
            Transition::Exec => self.data.yellow.into(),
            Transition::Platform => self.data.purple.into(),
            Transition::Flag => self.data.green.into(),
            Transition::Split => bviz_ui_theme::mix(self.data.purple, self.data.red, SPLIT_BLEND),
            Transition::Changed => self.label,
        }
    }

    /// The ink of a status badge's glyph.
    fn status_color(&self, status: Status) -> Rgba {
        match status {
            Status::Succeeded => self.colors.success,
            Status::Skipped | Status::Unfinished => self.colors.text_muted,
            Status::Flaky => self.colors.warning,
            Status::TestFailed | Status::Failed | Status::RootCause => self.colors.error,
        }
        .into()
    }

    /// [`MARK_LIFT`] signed to point away from the canvas: up on a theme dark
    /// enough to take white text, down on one that takes black.
    fn mark_lift(&self) -> f32 {
        match bviz_ui_theme::label_on(self.background).l > 0.5 {
            true => MARK_LIFT,
            false => -MARK_LIFT,
        }
    }

    /// [`BRIDGE_DIM`] signed to point toward the canvas, which is the way a mark
    /// does not carry a connector.
    fn bridge_dim(&self) -> f32 {
        -BRIDGE_DIM.copysign(self.mark_lift())
    }

    /// The wash a marked box's fill is painted over, at `strength` of the full
    /// mark.
    fn highlight_wash(&self, strength: f32) -> Rgba {
        Rgba {
            a: HIGHLIGHT_WASH * strength,
            ..self.highlight
        }
    }
}

/// What a frame fills its boxes by: the colouring chosen, with what that
/// colouring reads.
pub(crate) enum Fills<'a> {
    Kind,
    Time(&'a NodeTimes),
    Runner(&'a NodeTimes),
    Playback(&'a NodeStates),
}

impl<'a> Fills<'a> {
    /// The fills `colouring` gives the boxes of a graph whose nodes carry
    /// `times` and `states`. A colouring by what the graph does not carry
    /// fills by kind.
    pub(crate) fn of(
        colouring: Colouring,
        times: Option<&'a NodeTimes>,
        states: Option<&'a NodeStates>,
    ) -> Self {
        match (colouring, times, states) {
            (Colouring::Time, Some(times), _) => Fills::Time(times),
            (Colouring::Runner, Some(times), _) => Fills::Runner(times),
            (Colouring::Playback, _, Some(states)) => Fills::Playback(states),
            _ => Fills::Kind,
        }
    }

    /// The fill of `b`, where a playback stands at `at`.
    fn fill(&self, b: &bviz_graph_layout::Box, at: Option<EpochMicros>, palette: &Palette) -> Rgba {
        match self {
            Fills::Kind => palette.kind_fill(b.kind, b.compound),
            Fills::Time(times) => match times.get(b.id).zip(times.extent(b.kind)) {
                Some((timed, extent)) => {
                    let fill = palette.time_fill(extent.place(timed.held.total().wall));
                    palette.held_fill(b.kind, b.compound, fill)
                }
                None => palette.untimed_fill(b.kind, b.compound),
            },
            Fills::Runner(times) => match times.get(b.id) {
                Some(timed) => {
                    let fill = palette.ran_fill(Ran::of(&timed.held));
                    palette.held_fill(b.kind, b.compound, fill)
                }
                None => palette.untimed_fill(b.kind, b.compound),
            },
            Fills::Playback(states) => match at.and_then(|at| states.at(b.id, at)) {
                Some(state) => palette.held_fill(b.kind, b.compound, palette.state_fill(state)),
                None => palette.untimed_fill(b.kind, b.compound),
            },
        }
    }
}

/// A disclosure title as a frame painted it, by which a gesture landing on the
/// title reaches the box it names.
pub(crate) struct TitleTarget {
    plate: Bounds<Pixels>,
    id: Id,
}

/// A box the frame draws: the rect it occupies and the opacity it paints at,
/// both in layout pixels.
struct Placed<'a> {
    b: &'a bviz_graph_layout::Box,
    rect: Rect,
    alpha: f32,
    /// On its way out of the drawing, which only the morph still holds it for.
    leaving: bool,
}

impl Placed<'_> {
    /// Whether this box draws a disclosure caret. A box on its way out is absent
    /// from the drawing that replaced it, so there is nothing left there to open
    /// or close and it carries none however the layout it is leaving classified
    /// it.
    fn expandable(&self, expandable: &Expandable) -> bool {
        !self.leaving && expandable.holds(self.b)
    }
}

/// A box resolved against the theme and the hover's marked set, in layout
/// pixels.
struct PaintBox<'a> {
    id: Id,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    compound: bool,
    /// Whether this box can be opened or closed, so the renderer draws a
    /// disclosure caret: down when open (compound), right when collapsed.
    expandable: bool,
    /// The themed fill, which the quad and the plate a label sits on are both
    /// painted in.
    fill: Rgba,
    label: &'a str,
    /// How strongly the box wears the accent border, from 0 for none to 1: full
    /// while it is the selected box, and falling away as the emphasis on a
    /// just-toggled box fades.
    highlight: f32,
    /// Whether a marking picks this box out, which the wash over its fill says.
    marked: bool,
    /// Opacity the whole box paints at: a morph fades it up from zero for a box
    /// only the new drawing holds and down to zero for one only the old one
    /// held, and a standing selection holds it down for a box its marking leaves
    /// out.
    alpha: f32,
    status: Option<Status>,
    /// Whether the box stands for a step of the critical path, which a halo
    /// around it says.
    critical: bool,
}

impl<'a> PaintBox<'a> {
    fn of(
        p: &Placed<'a>,
        scene: &Scene,
        marks: &Marks,
        statuses: &Statuses,
        fills: &Fills,
        critical: Option<&CriticalNodes>,
    ) -> Self {
        let Placed { b, rect, alpha, .. } = *p;
        let marked = marks.marks_box(b.id);
        let alpha = alpha * receded(marks.strength(marked), scene.filter_strength(b.id));
        let fill = fills.fill(b, scene.at, &scene.palette);
        let toggled = match scene.highlight {
            Some((id, alpha)) if id == b.id => alpha,
            _ => 0.0,
        };
        // A selection outlasts the fade, so the box it holds keeps the border at
        // full whatever the emphasis on it has come down to.
        let accent = match scene.selected == Some(b.id) {
            true => 1.0,
            false => toggled,
        };
        PaintBox {
            id: b.id,
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
            compound: b.compound,
            expandable: p.expandable(&scene.expandable),
            fill: faded(fill, alpha),
            label: &b.label,
            highlight: accent,
            marked,
            alpha,
            status: statuses.get(b.id),
            critical: critical.is_some_and(|nodes| nodes.marks(b.id, !b.compound)),
        }
    }

    /// The badge of a box drawn without its label. A container's keeps to
    /// its title band, the rest being ground the boxes it holds are drawn on,
    /// and a leaf's other place is the corner a title floated over the leaf
    /// from the top of its container reaches last.
    fn bare_badge(&self, rect: Bounds<Pixels>, zoom: f32) -> Option<BareBadge> {
        let status = self.status?;
        let half = status_half(zoom);
        let reach = px(half + STATUS_RIM);
        let (center, otherwise) = match self.compound {
            true => (rect.origin + point(reach, reach), None),
            false => {
                let corner = point(rect.left() + reach, rect.bottom() - reach);
                let holds = rect.size.width >= reach * 2. && rect.size.height >= reach * 2.;
                (rect.center(), holds.then_some(corner))
            }
        };
        Some(BareBadge {
            badge: StatusBadge {
                center,
                half,
                status,
                alpha: self.alpha,
            },
            otherwise,
        })
    }

    /// The wash a marking lays over this box's fill, transparent where the box
    /// is unmarked.
    fn wash(&self, palette: &Palette) -> Rgba {
        palette.highlight_wash(if self.marked { self.alpha } else { 0.0 })
    }
}

/// Paints a connector's provenances as evenly spaced parallel lanes centered on
/// its route, `marked` where a marking has picked the connector out.
fn paint_lanes(
    c: &Connector,
    marked: bool,
    alpha: f32,
    bounds: Bounds<Pixels>,
    camera: Camera,
    palette: &Palette,
    window: &mut Window,
) {
    let z = camera.zoom;
    let screen: Vec<(f32, f32)> = c
        .points
        .iter()
        .map(|(x, y)| {
            let s = to_screen(*x, *y, bounds, camera);
            (f32::from(s.x), f32::from(s.y))
        })
        .collect();
    let n = c.kinds.len() as f32;
    for (i, kind) in c.kinds.iter().enumerate() {
        let slot = i as f32 - (n - 1.0) / 2.0;
        let lane = lane_polyline(&screen, slot * PARALLEL_SPACING * z);
        paint_connector(&lane, *kind, marked, alpha, z, palette, window);
    }
}

/// Sets the diamond marking `transition` halfway along `c`'s route.
fn paint_badge(
    c: &Connector,
    transition: Transition,
    alpha: f32,
    bounds: Bounds<Pixels>,
    camera: Camera,
    palette: &Palette,
    window: &mut Window,
) {
    let Some((x, y)) = midway(&c.points) else {
        return;
    };
    let center = to_screen(x, y, bounds, camera);
    let half = (BADGE_HALF * camera.zoom).max(BADGE_FLOOR);
    let rim = faded(palette.background, alpha);
    paint_diamond(center, half + BADGE_RIM, rim, window);
    let fill = faded(palette.transition_color(transition), alpha);
    paint_diamond(center, half, fill, window);
}

/// The corners of a square standing on one corner, about its middle, for a
/// half-diagonal of one.
const DIAMOND: [(f32, f32); 4] = [(0.0, -1.0), (1.0, 0.0), (0.0, 1.0), (-1.0, 0.0)];

/// Fills the square standing on one corner, centred on `center`, whose corners
/// lie `half` from it.
pub(crate) fn paint_diamond(center: Point<Pixels>, half: f32, color: Rgba, window: &mut Window) {
    let corners = DIAMOND.map(|(x, y)| point(center.x + px(x * half), center.y + px(y * half)));
    let mut diamond = PathBuilder::fill();
    diamond.add_polygon(&corners, true);
    if let Ok(path) = diamond.build() {
        paint_path(path, color, window);
    }
}

/// The shape a status badge is drawn as. Each status takes one no other does,
/// so a reader who cannot tell their inks apart still tells the statuses
/// apart. None is the diamond of a transition badge, which a theme may give an
/// ink a status wears and which is rimmed and floored alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Glyph {
    Disc,
    Bar,
    Ring,
    TriangleUp,
    Square,
    TriangleDown,
    Cross,
}

impl Glyph {
    fn of(status: Status) -> Self {
        match status {
            Status::Succeeded => Glyph::Disc,
            Status::Skipped => Glyph::Bar,
            Status::Unfinished => Glyph::Ring,
            Status::Flaky => Glyph::TriangleUp,
            Status::TestFailed => Glyph::Square,
            Status::Failed => Glyph::TriangleDown,
            Status::RootCause => Glyph::Cross,
        }
    }
}

/// The corners of a triangle standing on its base, about its middle, for a
/// glyph of half-width one.
const TRIANGLE: [(f32, f32); 3] = [
    (0.0, -TRIANGLE_APEX),
    (1.0, TRIANGLE_BASE),
    (-1.0, TRIANGLE_BASE),
];

/// [`TRIANGLE`] hanging from its base.
const TRIANGLE_DOWN: [(f32, f32); 3] = [
    (0.0, TRIANGLE_APEX),
    (-1.0, -TRIANGLE_BASE),
    (1.0, -TRIANGLE_BASE),
];

/// The corners of a cross lying on the diagonals, about its middle, for a
/// glyph of half-width one: an upright cross of arms reaching one from the
/// middle, turned an eighth of a turn.
fn cross() -> [(f32, f32); 12] {
    let t = CROSS_ARM;
    [
        (-t, -1.0),
        (t, -1.0),
        (t, -t),
        (1.0, -t),
        (1.0, t),
        (t, t),
        (t, 1.0),
        (-t, 1.0),
        (-t, t),
        (-1.0, t),
        (-1.0, -t),
        (-t, -t),
    ]
    .map(|(x, y)| {
        (
            (x - y) * std::f32::consts::FRAC_1_SQRT_2,
            (x + y) * std::f32::consts::FRAC_1_SQRT_2,
        )
    })
}

/// The screen half-width a status glyph is drawn at where the label it goes
/// with, or the box it stands for, is drawn at `scale` of its layout size.
fn status_half(scale: f32) -> f32 {
    (STATUS_HALF * scale).max(STATUS_FLOOR)
}

/// The screen width held for a status badge left of a label drawn at `scale`
/// of its layout size: the glyph, its rim to either side, and the gap before
/// what follows.
fn status_reserve(scale: f32) -> f32 {
    2. * (status_half(scale) + STATUS_RIM) + STATUS_GAP * scale
}

/// A status badge a frame draws, in screen pixels.
#[derive(Clone, Copy)]
struct StatusBadge {
    center: Point<Pixels>,
    half: f32,
    status: Status,
    /// The opacity of the box the badge belongs to, which it fades with.
    alpha: f32,
}

impl StatusBadge {
    /// The square the badge is drawn within, rim and all.
    fn bounds(&self) -> Bounds<Pixels> {
        let reach = px(self.half + STATUS_RIM);
        Bounds::new(
            point(self.center.x - reach, self.center.y - reach),
            size(reach * 2., reach * 2.),
        )
    }

    fn paint(&self, palette: &Palette, window: &mut Window) {
        paint_status(
            self.center,
            self.half,
            self.status,
            self.alpha,
            palette,
            window,
        );
    }
}

/// The badge of a box drawn without its label, which stands only where the
/// ground is free.
struct BareBadge {
    /// The badge where it stands by choice.
    badge: StatusBadge,
    /// Where it stands with that ground taken, for a box that has such a
    /// place.
    otherwise: Option<Point<Pixels>>,
}

/// Side of the squares [`BadgeGround`] finds badges by: the extent of a badge
/// held at its floor.
const BADGE_CELL: f32 = 2.0 * (STATUS_FLOOR + STATUS_RIM);

/// The cells a badge held at its floor reaches at most.
const CELLS_PER_BADGE: usize = 4;

/// The ground the status badges of one frame have taken.
struct BadgeGround {
    /// Each bounds taken, once for every cell it reaches, with the entry
    /// that cell held before it.
    taken: Vec<(Bounds<Pixels>, Option<usize>)>,
    /// The entry of `taken` each cell held last.
    cells: FxHashMap<(i32, i32), usize>,
}

impl BadgeGround {
    /// Ground with room for `badges` of them.
    fn with_capacity(badges: usize) -> Self {
        let entries = badges * CELLS_PER_BADGE;
        Self {
            taken: Vec::with_capacity(entries),
            cells: FxHashMap::with_capacity_and_hasher(entries, Default::default()),
        }
    }

    fn cells(bounds: Bounds<Pixels>) -> impl Iterator<Item = (i32, i32)> {
        let cell = |at: Pixels| (f32::from(at) / BADGE_CELL).floor() as i32;
        let (from, to) = (bounds.origin, bounds.bottom_right());
        (cell(from.x)..=cell(to.x))
            .flat_map(move |x| (cell(from.y)..=cell(to.y)).map(move |y| (x, y)))
    }

    /// Whether `bounds` lands on none of the bounds taken.
    fn is_free(&self, bounds: Bounds<Pixels>) -> bool {
        Self::cells(bounds).all(|cell| {
            let mut entry = self.cells.get(&cell).copied();
            while let Some((taken, before)) = entry.map(|entry| self.taken[entry]) {
                if taken.intersects(&bounds) {
                    return false;
                }
                entry = before;
            }
            true
        })
    }

    fn take(&mut self, bounds: Bounds<Pixels>) {
        for cell in Self::cells(bounds) {
            let before = self.cells.insert(cell, self.taken.len());
            self.taken.push((bounds, before));
        }
    }
}

/// Adds to `placed` each of `bare` that finds free ground. A container stands
/// at least as severe as any box it holds and is walked before it, so placing
/// the most severe first leaves its badge standing where the two would meet.
fn place_bare(mut bare: Vec<BareBadge>, floated: &[Bounds<Pixels>], placed: &mut Vec<StatusBadge>) {
    let mut ground = BadgeGround::with_capacity(placed.len() + bare.len());
    for badge in placed.iter() {
        ground.take(badge.bounds());
    }
    bare.sort_by_key(|bare| Reverse(bare.badge.status));
    for bare in bare {
        let standing = std::iter::once(bare.badge.center)
            .chain(bare.otherwise)
            .map(|center| StatusBadge {
                center,
                ..bare.badge
            })
            .find(|badge| {
                let bounds = badge.bounds();
                ground.is_free(bounds) && !floated.iter().any(|taken| taken.intersects(&bounds))
            });
        if let Some(badge) = standing {
            ground.take(badge.bounds());
            placed.push(badge);
        }
    }
}

/// Paints the badge of `status` about `center`: its glyph, `half` to either
/// side, over a rim of the canvas's colour.
pub(crate) fn paint_status(
    center: Point<Pixels>,
    half: f32,
    status: Status,
    alpha: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let rim = faded(palette.background, alpha);
    let ink = faded(palette.status_color(status), alpha);
    // A shape of upright sides or of a round one is a quad, whose coverage
    // is computed rather than sampled; the rest are paths.
    let mut block = |half_width: f32, half_height: f32, corner: f32| {
        for (grow, color) in [(STATUS_RIM, rim), (0.0, ink)] {
            let (w, h) = (half_width + grow, half_height + grow);
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - px(w), center.y - px(h)),
                    size(px(2. * w), px(2. * h)),
                ),
                px(if corner > 0.0 { corner + grow } else { 0.0 }),
                color,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    };
    match Glyph::of(status) {
        Glyph::Disc => block(half, half, half),
        Glyph::Bar => block(half, half * BAR_HALF_HEIGHT, 0.0),
        Glyph::Square => block(half * SQUARE_HALF_SIDE, half * SQUARE_HALF_SIDE, 0.0),
        Glyph::Ring => {
            let reach = half + STATUS_RIM;
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - px(reach), center.y - px(reach)),
                    size(px(2. * reach), px(2. * reach)),
                ),
                px(reach),
                rim,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
            window.paint_quad(quad(
                Bounds::new(
                    point(center.x - px(half), center.y - px(half)),
                    size(px(2. * half), px(2. * half)),
                ),
                px(half),
                Hsla::transparent_black(),
                px((half * RING_STROKE).max(RING_STROKE_FLOOR)),
                Hsla::from(ink),
                BorderStyle::Solid,
            ));
        }
        Glyph::TriangleUp => paint_rimmed(TRIANGLE, center, half, rim, ink, window),
        Glyph::TriangleDown => paint_rimmed(TRIANGLE_DOWN, center, half, rim, ink, window),
        Glyph::Cross => paint_rimmed(cross(), center, half, rim, ink, window),
    }
}

/// Fills the polygon `unit` gives for a half-width of one, scaled to `half`
/// about `center`, in `ink` over a `rim` standing [`STATUS_RIM`] out from it.
fn paint_rimmed<const N: usize>(
    unit: [(f32, f32); N],
    center: Point<Pixels>,
    half: f32,
    rim: Rgba,
    ink: Rgba,
    window: &mut Window,
) {
    let scaled = unit.map(|(x, y)| (x * half, y * half));
    for (outline, color) in [(outset(scaled, STATUS_RIM), rim), (scaled, ink)] {
        let corners = outline.map(|(x, y)| point(center.x + px(x), center.y + px(y)));
        let mut shape = PathBuilder::fill();
        shape.add_polygon(&corners, true);
        if let Ok(path) = shape.build() {
            paint_path(path, color, window);
        }
    }
}

/// `polygon` with every side moved `by` outward along its normal, the corners
/// following to where the moved sides meet. No two consecutive sides of
/// `polygon` may run in opposite directions.
fn outset<const N: usize>(polygon: [(f32, f32); N], by: f32) -> [(f32, f32); N] {
    let n = N;
    let side = |i: usize| (polygon[i], polygon[(i + 1) % n]);
    let turning: f32 = (0..n)
        .map(|i| {
            let (a, b) = side(i);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    // The normal of the side leaving corner `i`, pointing out of the polygon
    // whichever way round its corners are listed.
    let normal = |i: usize| {
        let (a, b) = side(i);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy).max(f32::EPSILON) * turning.signum();
        (dy / length, -dx / length)
    };
    std::array::from_fn(|i| {
        let (before, after) = (normal((i + n - 1) % n), normal(i));
        let reach = by / (1.0 + before.0 * after.0 + before.1 * after.1);
        (
            polygon[i].0 + (before.0 + after.0) * reach,
            polygon[i].1 + (before.1 + after.1) * reach,
        )
    })
}

/// The point halfway along the polyline `points` by length; `None` for a route
/// with no points.
fn midway(points: &[(f64, f64)]) -> Option<(f64, f64)> {
    let first = *points.first()?;
    let legs = || {
        points.windows(2).map(|leg| {
            let (a, b) = (leg[0], leg[1]);
            (a, b, (b.0 - a.0).hypot(b.1 - a.1))
        })
    };
    let mut left = legs().map(|(_, _, length)| length).sum::<f64>() / 2.0;
    for (a, b, length) in legs() {
        if left <= length && length > 0.0 {
            let t = left / length;
            return Some((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
        left -= length;
    }
    Some(first)
}

/// Where `b` falls in the window under `camera`.
fn screen_rect(b: &PaintBox, bounds: Bounds<Pixels>, camera: Camera) -> Bounds<Pixels> {
    let origin = to_screen(b.x, b.y, bounds, camera);
    let zoom = camera.zoom;
    Bounds::new(origin, size(px(b.w as f32 * zoom), px(b.h as f32 * zoom)))
}

/// Paints a box's filled quad, over which a marking's wash goes.
fn paint_box(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let corner = px(BOX_CORNER * zoom);
    let ((border_w, border_c), mark) = box_emphasis(b, zoom, palette, rect, corner);
    window.paint_quad(quad(
        rect,
        corner,
        b.fill,
        border_w,
        border_c,
        BorderStyle::Solid,
    ));
    mark(window);
}

/// The halo around a box standing for a step of the critical path, outside
/// the box so that it takes none of the channels a box's own emphases do. It
/// is drawn in the label ink, which every fill keeps a label legible against,
/// so it parts from whatever fill a colouring gives the box. Painted over the
/// routes, which meet the box at its border, and under the labels.
fn paint_critical_halo(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    let width = px((CRITICAL_HALO * zoom).max(CRITICAL_HALO_FLOOR));
    let around = Bounds {
        origin: rect.origin - point(width, width),
        size: size(rect.size.width + width * 2., rect.size.height + width * 2.),
    };
    window.paint_quad(quad(
        around,
        px(BOX_CORNER * zoom) + width,
        Hsla::transparent_black(),
        width,
        Hsla::from(faded(palette.label, b.alpha)),
        BorderStyle::Solid,
    ));
}

/// A box's emphases, told apart by the channel each takes: the quad border to
/// stroke, which the accent carries from the themed border to itself and from a
/// normal to a thick stroke, and a deferred paint of a marking's wash, which
/// the fill alone carries.
fn box_emphasis(
    b: &PaintBox,
    zoom: f32,
    palette: &Palette,
    rect: Bounds<Pixels>,
    corner: Pixels,
) -> ((Pixels, Hsla), impl FnOnce(&mut Window)) {
    let (border_w, border_c) = if b.highlight > 0.0 {
        let normal = px(1.);
        let thick = px((2. * zoom).max(2.));
        let accent = Rgba {
            a: b.highlight,
            ..palette.highlight
        };
        (
            normal + (thick - normal) * b.highlight,
            Hsla::from(faded(accent, b.alpha)),
        )
    } else {
        (px(1.), Hsla::from(faded(palette.border, b.alpha)))
    };
    let wash = b.wash(palette);
    let mark = move |window: &mut Window| {
        if wash.a > 0.0 {
            window.paint_quad(quad(
                rect,
                corner,
                wash,
                px(0.),
                Hsla::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    };
    ((border_w, border_c), mark)
}

/// Where a box's label falls within it, in screen pixels: centered in the strip
/// its own box keeps for a label — a container's title band, or the whole box
/// for anything else — which no other box's label reaches into.
struct LabelPlacement {
    /// Font size the label is drawn at.
    size: f32,
    /// Distance from the box's top edge to the label's top.
    top: f32,
    /// Width held left of the label for a disclosure caret; `None` for a box that
    /// carries none. The caret keeps the proportion the layout reserved for it
    /// whatever size the label is drawn at.
    caret: Option<f32>,
    /// The status whose badge the label holds room for at its left end, ahead
    /// of the caret.
    status: Option<Status>,
    /// Width the label text is truncated to.
    avail: f32,
    /// Whether the title is lifted to the top of its box, over ground the boxes
    /// inside it are drawn on, which a container whose band the zoom has shrunk
    /// below the floored label does.
    floats: bool,
}

impl LabelPlacement {
    /// Where `b`'s label falls in the `rect` it is drawn at, or `None` if that
    /// rect is too short or too narrow for the floored label — what survives
    /// truncation in a narrower one names nothing, so the box is left bare.
    fn of(b: &PaintBox, rect: Bounds<Pixels>, zoom: f32) -> Option<Self> {
        let (w, h) = (f32::from(rect.size.width), f32::from(rect.size.height));
        let size = (LABEL_SIZE as f32 * zoom).max(LABEL_FLOOR);
        let needed = size + 2. * LABEL_PAD;
        if h < needed {
            return None;
        }
        let band = LABEL_BAND as f32 * zoom;
        let (top, floats) = match b.compound {
            true if band >= needed => ((band - size) / 2., false),
            true => (LABEL_PAD, true),
            false => ((h - size) / 2., false),
        };
        let caret = b
            .expandable
            .then(|| size * (CARET_RESERVE / LABEL_SIZE) as f32);
        let placement = LabelPlacement {
            size,
            top,
            caret,
            status: b.status,
            avail: 0.,
            floats,
        };
        let avail = w - 2. * LABEL_PAD - placement.reserved();
        (avail >= LABEL_MIN_WIDTH).then_some(LabelPlacement { avail, ..placement })
    }

    /// What the label is drawn at of its layout size.
    fn scale(&self) -> f32 {
        self.size / LABEL_SIZE as f32
    }

    /// Width the badge takes out of the plate, zero where the box carries none.
    fn badge_reserved(&self) -> f32 {
        self.status.map_or(0., |_| status_reserve(self.scale()))
    }

    /// Width the badge and the caret take out of the plate ahead of the text.
    fn reserved(&self) -> f32 {
        self.badge_reserved() + self.caret.unwrap_or(0.)
    }

    /// The plate a `text`-wide label fills in `rect`: the badge, the caret and
    /// the text centered together as one unit, with the label's clearance
    /// around them.
    fn plate(&self, rect: Bounds<Pixels>, text: f32) -> Bounds<Pixels> {
        let unit = self.reserved() + text;
        let left = f32::from(rect.origin.x) + (f32::from(rect.size.width) - unit) / 2.;
        Bounds::new(
            point(
                px(left - LABEL_PAD),
                px(f32::from(rect.origin.y) + self.top - LABEL_PAD),
            ),
            size(px(unit + 2. * LABEL_PAD), px(self.size + 2. * LABEL_PAD)),
        )
    }

    /// Where the caret sits in `plate`: its left edge, at the label's vertical
    /// center.
    fn caret_at(&self, plate: Bounds<Pixels>) -> Point<Pixels> {
        point(
            plate.origin.x + px(LABEL_PAD + self.badge_reserved()),
            plate.origin.y + px(LABEL_PAD + self.size / 2.),
        )
    }

    /// The badge set at the left end of `plate`, at the label's vertical
    /// center, to be drawn at `alpha`; `None` for a box the build reports
    /// nothing of.
    fn badge(&self, plate: Bounds<Pixels>, alpha: f32) -> Option<StatusBadge> {
        let status = self.status?;
        let half = status_half(self.scale());
        Some(StatusBadge {
            center: point(
                plate.origin.x + px(LABEL_PAD + STATUS_RIM + half),
                plate.origin.y + px(LABEL_PAD + self.size / 2.),
            ),
            half,
            status,
            alpha,
        })
    }

    /// Where the text begins in `plate`: its top left corner, past the badge
    /// and the caret.
    fn text_at(&self, plate: Bounds<Pixels>) -> Point<Pixels> {
        plate.origin + point(px(LABEL_PAD + self.reserved()), px(LABEL_PAD))
    }

    /// The handle box `id`'s label is, once drawn on `plate`; `None` for a box
    /// that carries no disclosure caret.
    fn title(&self, plate: Bounds<Pixels>, id: Id) -> Option<TitleTarget> {
        self.caret.map(|_| TitleTarget { plate, id })
    }
}

/// The box a title painted over `pos` (window px) opens or closes.
pub(crate) fn title_at(titles: &[TitleTarget], pos: Point<Pixels>) -> Option<Id> {
    titles.iter().find(|t| t.plate.contains(&pos)).map(|t| t.id)
}

/// What a painted label leaves for the passes after it.
struct PaintedLabel {
    title: Option<TitleTarget>,
    badge: Option<StatusBadge>,
}

/// Paints a box's label, truncated to the width there is room for, with a
/// disclosure caret for an expandable box, and reports the title a click on it
/// toggles and the status badge the label holds room for; `None` where no
/// label is painted. The font is the window's inherited UI text style so the
/// views stay visually coherent, and a container's title is bold.
///
/// `floated` collects what the floating titles have taken, and a label that
/// would land on any of it is dropped rather than printed over it. Since the
/// caller walks the boxes outermost first, that leaves the coarser, more telling
/// name standing.
fn paint_box_label(
    b: &PaintBox,
    rect: Bounds<Pixels>,
    zoom: f32,
    palette: &Palette,
    floated: &mut Vec<Bounds<Pixels>>,
    window: &mut Window,
    cx: &mut App,
) -> Option<PaintedLabel> {
    let placement = LabelPlacement::of(b, rect, zoom)?;
    let fs = placement.size;
    let label_color = faded(bviz_ui_theme::label_on(b.fill).into(), b.alpha);
    let base = bviz_ui_assets::text_metrics(window).font;
    let font = if b.compound { base.bold() } else { base };
    pan_bench::tally(Tally::ShapedLabels, 1);
    let line = bviz_ui_components::shape_truncated(
        b.label,
        placement.avail,
        font,
        px(fs),
        label_color.into(),
        window,
    );
    let plate = placement.plate(rect, f32::from(line.width()));
    if floated.iter().any(|taken| taken.intersects(&plate)) {
        return None;
    }
    if placement.floats {
        floated.push(plate);
    }
    // The plate is painted in the box's own ground, wash and all, so a name reads
    // against its box rather than against the routes crossing it and leaves no
    // rectangle of its own behind.
    let ground = bviz_ui_theme::over(b.wash(palette), b.fill);
    window.paint_quad(quad(
        plate,
        px(0.),
        ground,
        px(0.),
        Hsla::transparent_black(),
        BorderStyle::Solid,
    ));
    let _ = line.paint(
        placement.text_at(plate),
        px(fs),
        TextAlign::Left,
        None,
        window,
        cx,
    );

    if let Some(caret) = placement.caret {
        // Phosphor icons render as `svg()` elements, which cannot be painted
        // inside this canvas, so the caret is a small filled triangle.
        paint_caret(
            placement.caret_at(plate),
            px(caret),
            px(fs),
            b.compound,
            label_color,
            window,
        );
    }
    Some(PaintedLabel {
        title: placement.title(plate, b.id),
        badge: placement.badge(plate, b.alpha),
    })
}

fn paint_path(path: Path<Pixels>, color: impl Into<Background>, window: &mut Window) {
    pan_bench::tally(Tally::PaintPaths, 1);
    window.paint_path(path, color);
}

/// Paints a small filled-triangle disclosure caret inside the reserved
/// `footprint` whose left edge is `at.x`, centered vertically at `at.y`. The
/// triangle width fills the footprint less a trailing gap before the label; its
/// height scales with the font size `fs`. `down` points it downward (expanded),
/// otherwise rightward (collapsed).
fn paint_caret(
    at: Point<Pixels>,
    footprint: Pixels,
    fs: Pixels,
    down: bool,
    color: Rgba,
    window: &mut Window,
) {
    let s = f32::from(footprint) * 0.6; // triangle width within the footprint
    let height = f32::from(fs) * 0.5; // triangle height, scaling with the label
    let left = at.x;
    let right = left + px(s);
    let (top, bottom) = (at.y - px(height / 2.), at.y + px(height / 2.));
    let mut tri = PathBuilder::fill();
    if down {
        // Apex down, base along the top.
        tri.add_polygon(
            &[
                point(left, top),
                point(right, top),
                point(left + px(s / 2.), bottom),
            ],
            true,
        );
    } else {
        // Apex right, base along the left.
        tri.add_polygon(
            &[point(left, top), point(right, at.y), point(left, bottom)],
            true,
        );
    }
    if let Ok(path) = tri.build() {
        paint_path(path, color, window);
    }
}

/// A polyline parallel to `pts` at signed perpendicular distance `d` (screen px),
/// for drawing a node pair's several provenances as evenly spaced parallel lanes.
/// Every vertex is pushed off its segments' normals — the endpoints perpendicular
/// to their one segment, the interior mitered at the corners — so a lane stays
/// parallel to the route along its whole length and meets each node offset
/// alongside its siblings rather than converging to a single point.
pub(crate) fn lane_polyline(pts: &[(f32, f32)], d: f32) -> Vec<(f32, f32)> {
    if d == 0.0 || pts.len() < 2 {
        return pts.to_vec();
    }
    let left_normal = |from: (f32, f32), to: (f32, f32)| {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let l = (dx * dx + dy * dy).sqrt().max(1e-3);
        (-dy / l, dx / l)
    };
    let n = pts.len();
    let mut out = Vec::with_capacity(n);
    let ns = left_normal(pts[0], pts[1]);
    out.push((pts[0].0 + ns.0 * d, pts[0].1 + ns.1 * d));
    for i in 1..n - 1 {
        out.push(miter_vertex(pts[i - 1], pts[i], pts[i + 1], d));
    }
    let ne = left_normal(pts[n - 2], pts[n - 1]);
    out.push((pts[n - 1].0 + ne.0 * d, pts[n - 1].1 + ne.1 * d));
    out
}

/// Offsets the corner `b` (between segments `a->b` and `b->c`) by `d` along the
/// miter direction, keeping it distance `d` from both segments. A near-reversal,
/// or a miter that would shoot far past a sharp corner, falls back to one
/// segment's normal.
fn miter_vertex(a: (f32, f32), b: (f32, f32), c: (f32, f32), d: f32) -> (f32, f32) {
    let unit = |(x, y): (f32, f32)| {
        let l = (x * x + y * y).sqrt().max(1e-3);
        (x / l, y / l)
    };
    let d1 = unit((b.0 - a.0, b.1 - a.1));
    let d2 = unit((c.0 - b.0, c.1 - b.1));
    let (n1, n2) = ((-d1.1, d1.0), (-d2.1, d2.0)); // left normals
    let m = (n1.0 + n2.0, n1.1 + n2.1);
    let ml = (m.0 * m.0 + m.1 * m.1).sqrt();
    if ml < 1e-3 {
        return (b.0 + n1.0 * d, b.1 + n1.1 * d);
    }
    let mdir = (m.0 / ml, m.1 / ml);
    let cos_half = (mdir.0 * n1.0 + mdir.1 * n1.1).max(0.35); // miter limit
    let scale = d / cos_half;
    (b.0 + mdir.0 * scale, b.1 + mdir.1 * scale)
}

/// Strokes a connector polyline (screen coordinates) and caps its end with a
/// filled arrowhead pointing along the final segment.
fn paint_connector(
    points: &[(f32, f32)],
    kind: EdgeKind,
    marked: bool,
    alpha: f32,
    zoom: f32,
    palette: &Palette,
    window: &mut Window,
) {
    if points.len() < 2 {
        return;
    }
    let color = faded(palette.edge_color(kind, marked), alpha);
    let pts: Vec<Point<Pixels>> = points.iter().map(|(x, y)| point(px(*x), px(*y))).collect();

    let n = pts.len();
    let tip = pts[n - 1];
    let (dir, seg) = direction(pts[n - 2], tip);
    let trim = stem_trim(zoom).min(seg);
    let line_end = point(tip.x - px(dir.0 * trim), tip.y - px(dir.1 * trim));

    let width = match marked {
        true => MARKED_WIDTH,
        false => CONNECTOR_WIDTH,
    };
    let mut line = PathBuilder::stroke(stroke_width(width, zoom));
    if kind == EdgeKind::Transitive {
        let (dash, gap) = bridge_pattern(zoom);
        line = line.dash_array(&[dash, gap]);
    }
    line.move_to(pts[0]);
    for p in &pts[1..n - 1] {
        line.line_to(*p);
    }
    line.line_to(line_end);
    if let Ok(path) = line.build() {
        paint_path(path, color, window);
    }
    paint_arrowhead(tip, dir, zoom, color, window);
}

/// The screen width a line of pre-zoom `width` is stroked at under `zoom`. The
/// floor lands on the zoom, which every width shares, so the widths hold their
/// ratio to one another and a marked line goes on reading as the heavier one. It
/// reaches the stroke alone: a route's course, the arrowhead capping it and the
/// spacing of a bundle's lanes all follow the camera, so a route goes on meeting
/// its boxes where the layout puts it.
fn stroke_width(width: f32, zoom: f32) -> Pixels {
    px(width * zoom.max(FLOORED_BELOW))
}

/// The dash and the gap a bridged route is drawn in at `zoom`, each held to
/// [`BRIDGE_PATTERN_FLOOR`] however far the camera is out. The arrowhead capping
/// the route is filled whole, so the direction it reports is not a dash the
/// pattern might have left out.
fn bridge_pattern(zoom: f32) -> (Pixels, Pixels) {
    (
        px((BRIDGE_DASH * zoom).max(BRIDGE_PATTERN_FLOOR)),
        px((BRIDGE_GAP * zoom).max(BRIDGE_PATTERN_FLOOR)),
    )
}

/// The share of its own opacity a connector is drawn at, standing for the share
/// of the drawn band the line itself accounts for: [`stroke_width`] holds the
/// stroke while the graph goes on shrinking under it, so the ink is taken back
/// down by as much as the held width adds. Bounded below by [`TAPER_FLOOR`].
fn ink_taper(zoom: f32) -> f32 {
    (zoom / FLOORED_BELOW).clamp(TAPER_FLOOR, 1.0)
}

/// The unit vector from `a` to `b`, and how far apart they are.
fn direction(a: Point<Pixels>, b: Point<Pixels>) -> ((f32, f32), f32) {
    let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    ((dx / len, dy / len), len)
}

/// How far back from the tip a connector's line stops.
fn stem_trim(zoom: f32) -> f32 {
    (ARROW_LENGTH - ARROW_OVERDRAW) as f32 * zoom
}

/// Paints the triangle [`arrowhead`] describes.
fn paint_arrowhead(
    tip: Point<Pixels>,
    dir: (f32, f32),
    zoom: f32,
    color: Rgba,
    window: &mut Window,
) {
    let mut head = PathBuilder::fill();
    head.add_polygon(&arrowhead(tip, dir, zoom), true);
    if let Ok(path) = head.build() {
        paint_path(path, color, window);
    }
}

/// The triangle capping a connector at `tip` and pointing along the unit vector
/// `dir`, sized as [`ARROW_LENGTH`] and [`ARROW_HALF_WIDTH`] give it at `zoom`.
fn arrowhead(tip: Point<Pixels>, dir: (f32, f32), zoom: f32) -> [Point<Pixels>; 3] {
    let (length, half_width) = (ARROW_LENGTH as f32 * zoom, ARROW_HALF_WIDTH as f32 * zoom);
    let (ux, uy) = dir;
    let (nx, ny) = (-uy, ux);
    let base = point(tip.x - px(ux * length), tip.y - px(uy * length));
    [
        tip,
        point(base.x + px(nx * half_width), base.y + px(ny * half_width)),
        point(base.x - px(nx * half_width), base.y - px(ny * half_width)),
    ]
}

#[cfg(test)]
mod tests {
    /// The view the route tests cull against.
    const VIEW: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 100.0,
    };

    /// A route along `points`, running between the boxes `Id(0)` and `Id(1)`.
    fn route(points: Vec<(f64, f64)>) -> Connector {
        Connector {
            points,
            kinds: vec![EdgeKind::Query],
            from: Id(0),
            to: Id(1),
            transition: None,
        }
    }

    /// A badge sits halfway along a route by length, whatever its bends, so a
    /// long leg carries it rather than the bend nearest the middle point.
    #[test]
    fn midway_falls_halfway_along_the_route() {
        assert_eq!(midway(&[]), None);
        assert_eq!(midway(&[(3.0, 4.0)]), Some((3.0, 4.0)));
        assert_eq!(midway(&[(0.0, 0.0), (10.0, 0.0)]), Some((5.0, 0.0)));
        assert_eq!(
            midway(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (2.0, 20.0)]),
            Some((2.0, 9.0))
        );
    }

    /// A route the view could not show whole is left out, so a zoom deep into a
    /// dense graph is not laid over with routes leaving for somewhere the reader
    /// cannot see.
    #[test]
    fn a_route_too_long_to_follow_is_left_out() {
        let none_drawn = HashSet::new();

        let local = route(vec![(10.0, 10.0), (40.0, 40.0)]);
        assert!(drawn_route(&local, &VIEW, &none_drawn));

        // Crosses the view, but runs far past what it can show.
        let far = route(vec![(-5000.0, 50.0), (5000.0, 50.0)]);
        assert!(!drawn_route(&far, &VIEW, &none_drawn));

        // Short enough to follow, but nowhere near the view.
        let elsewhere = route(vec![(9000.0, 9000.0), (9030.0, 9030.0)]);
        assert!(!drawn_route(&elsewhere, &VIEW, &none_drawn));
    }

    /// A box on screen at either end carries a route past the reach test, which
    /// alone would leave it out. A zoom that shrinks the view below the route's
    /// reach must not take the route with it while the box it leaves is still
    /// there to be read.
    #[test]
    fn a_route_is_drawn_from_a_box_on_screen_however_far_it_reaches() {
        let far = route(vec![(-5000.0, 50.0), (5000.0, 50.0)]);
        for anchored in [
            HashSet::from([Id(0)]),
            HashSet::from([Id(1)]),
            HashSet::from([Id(0), Id(1)]),
        ] {
            assert!(drawn_route(&far, &VIEW, &anchored));
        }
        assert!(!drawn_route(&far, &VIEW, &HashSet::new()));
    }

    /// The exemption spares a route the reach test, not the view: one that never
    /// touches the view has nothing to draw there whoever its boxes are.
    #[test]
    fn a_route_missing_the_view_is_left_out_though_its_boxes_are_on_screen() {
        let both = HashSet::from([Id(0), Id(1)]);
        let past = route(vec![(-5000.0, 9000.0), (5000.0, 9000.0)]);
        assert!(!drawn_route(&past, &VIEW, &both));
    }

    /// An orthogonal route can detour well outside the boxes it joins, so its
    /// extent overruns the view even with both ends inside it.
    #[test]
    fn a_route_that_bends_outside_the_view_is_drawn_from_a_box_on_screen() {
        let both = HashSet::from([Id(0), Id(1)]);
        let detour = route(vec![
            (10.0, 10.0),
            (10.0, 5000.0),
            (90.0, 5000.0),
            (90.0, 90.0),
        ]);
        assert!(drawn_route(&detour, &VIEW, &both));
    }

    use super::*;
    use crate::camera::from_screen;
    use crate::morph::Transition;
    use bviz_graph_layout::LaidOut;
    use gpui::rgb;
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Instant;

    /// The zooms the caret invariant is checked over, spanning both sides of the
    /// band width at which a container's title stops fitting in it.
    const ZOOMS: [f32; 9] = [0.05, 0.1, 0.2, 0.4, 0.5, 0.6, 0.8, 1.0, 2.0];

    fn lbox(
        id: u32,
        label: &str,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        compound: bool,
    ) -> bviz_graph_layout::Box {
        bviz_graph_layout::Box {
            id: Id(id),
            label: label.into(),
            kind: Kind::Source,
            x,
            y,
            width: w,
            height: h,
            compound,
            group: false,
        }
    }

    /// A container large enough to carry a title at every zoom in [`ZOOMS`],
    /// holding one child over all of it but the title band — so a title floated
    /// out of that band lands wholly on the child, and the container's only
    /// ground of its own is the band itself.
    fn covered_container() -> LaidOut {
        let (w, h) = (4000.0, 3000.0);
        LaidOut {
            boxes: vec![
                lbox(0, "container", 0.0, 0.0, w, h, true),
                lbox(1, "member", 0.0, LABEL_BAND, w, h - LABEL_BAND, false),
            ],
            ..Default::default()
        }
    }

    /// A box as a frame draws it, where the theme and the marked set a full
    /// [`PaintBox::of`] resolves have no bearing on what is measured.
    fn paint_box_of<'a>(p: &Placed<'a>, expandable: &Expandable) -> PaintBox<'a> {
        PaintBox {
            id: p.b.id,
            x: p.rect.x,
            y: p.rect.y,
            w: p.rect.w,
            h: p.rect.h,
            compound: p.b.compound,
            expandable: p.expandable(expandable),
            fill: rgb(0),
            label: &p.b.label,
            highlight: 0.0,
            marked: false,
            alpha: p.alpha,
            status: None,
            critical: false,
        }
    }

    /// The boxes of `g` as a frame under `morph` draws them, placed as
    /// [`canvas_body`] places them.
    fn placed<'a>(g: &'a LaidOut, morph: Option<&Frame>) -> Vec<Placed<'a>> {
        g.boxes
            .iter()
            .map(|b| {
                let (rect, alpha) = morph.map_or((Rect::of(b), 1.0), |m| m.place(b));
                Placed {
                    b,
                    rect,
                    alpha,
                    leaving: false,
                }
            })
            .collect()
    }

    /// Which boxes of [`covered_container`] open and close: both, as a container
    /// and the collapsed group filling it do, so a click landing on the wrong one
    /// still toggles something; and the container alone, as one holding a plain
    /// leaf does.
    fn expansion_sets() -> [Expandable; 2] {
        [
            Expandable::Containers(Arc::new(HashSet::from([Id(0), Id(1)]))),
            Expandable::Containers(Arc::new(HashSet::from([Id(0)]))),
        ]
    }

    /// A caret a frame painted: the box it belongs to, the point at the middle of
    /// its footprint, and whether the title carrying it floated out of its band.
    struct DrawnCaret {
        id: Id,
        at: Point<Pixels>,
        floats: bool,
    }

    /// The titles a frame drawing `boxes` records under `camera`, and a caret for each,
    /// following the same order and overlap arbitration [`canvas_body`] paints
    /// in. Every label is measured at the widest its box has room for, which puts
    /// the plate at its widest and the caret furthest from the box's own center.
    fn drawn_titles(
        boxes: &[Placed],
        expandable: &Expandable,
        viewport: Bounds<Pixels>,
        camera: Camera,
    ) -> (Vec<TitleTarget>, Vec<DrawnCaret>) {
        let mut floated: Vec<Bounds<Pixels>> = Vec::new();
        let mut titles: Vec<TitleTarget> = Vec::new();
        let mut carets: Vec<DrawnCaret> = Vec::new();
        for p in boxes {
            let pb = paint_box_of(p, expandable);
            let rect = screen_rect(&pb, viewport, camera);
            let Some(placement) = LabelPlacement::of(&pb, rect, camera.zoom) else {
                continue;
            };
            let plate = placement.plate(rect, placement.avail);
            if floated.iter().any(|taken| taken.intersects(&plate)) {
                continue;
            }
            if placement.floats {
                floated.push(plate);
            }
            let Some(title) = placement.title(plate, p.b.id) else {
                continue;
            };
            carets.push(DrawnCaret {
                id: p.b.id,
                at: placement.caret_at(plate) + point(px(placement.reserved() / 2.), px(0.)),
                floats: placement.floats,
            });
            titles.push(title);
        }
        (titles, carets)
    }

    /// A viewport laid out away from the window origin, so a resolution that
    /// neglects to take the canvas offset off the cursor lands visibly wrong.
    fn viewport() -> Bounds<Pixels> {
        Bounds::new(point(px(10.), px(20.)), size(px(800.), px(600.)))
    }

    #[test]
    fn a_drawn_title_resolves_to_the_box_it_names() {
        for expandable in expansion_sets() {
            check_titles_resolve(&expandable);
        }
    }

    /// Asserts that every title drawn over [`covered_container`] both toggles and
    /// hovers the box it names, at every zoom in [`ZOOMS`].
    fn check_titles_resolve(expandable: &Expandable) {
        let g = covered_container();
        let (mut floating, mut banded, mut covered) = (0, 0, 0);
        for zoom in ZOOMS {
            let camera = Camera {
                zoom,
                pan: (0.0, 0.0),
            };
            let (titles, carets) = drawn_titles(&placed(&g, None), expandable, viewport(), camera);
            for caret in carets {
                let DrawnCaret { id, at, floats } = caret;
                match floats {
                    true => floating += 1,
                    false => banded += 1,
                }
                let resolved = crate::box_at(&titles, &g, None, viewport(), camera, at);
                assert_eq!(
                    resolved.map(|b| b.id),
                    Some(id),
                    "zoom {zoom}: the caret drawn for {id:?} resolves to another box"
                );
                // What the click keeps of that: a box it holds no handle on is
                // left alone, and every title target names one it does.
                assert_eq!(
                    resolved.filter(|b| expandable.holds(b)).map(|b| b.id),
                    Some(id),
                    "zoom {zoom}: the caret drawn for {id:?} toggles nothing"
                );
                let (gx, gy) = from_screen(at, viewport().origin, camera);
                covered +=
                    (crate::hit_box(&g.boxes, None, gx, gy).map(|b| b.id) != Some(id)) as u32;
            }
        }
        assert!(floating > 0, "no zoom floated a title out of its band");
        assert!(banded > 0, "no zoom left a title in its band");
        assert!(
            covered > 0,
            "no caret fell on a box other than its own, so the boxes alone \
            would have resolved every gesture and the case this pins never arose"
        );
    }

    /// How far a relayout slides [`covered_container`] for the mid-morph frame:
    /// wide enough that a box the morph places falls clear of every rect the
    /// layout laid out, so resolving a gesture against the layout alone lands on
    /// a different box or on none.
    const SLIDE: f64 = 6000.0;

    #[test]
    fn a_title_drawn_mid_morph_resolves_to_the_box_it_names() {
        let g = covered_container();
        let expandable = Expandable::Containers(Arc::new(HashSet::from([Id(0), Id(1)])));
        let t = Arc::new(
            Transition::between(Arc::new(slid(&g, SLIDE)), &g, None).expect("nothing to animate"),
        );
        // The morph's opening frame, which draws every box a whole slide away
        // from where the layout put it.
        let frame = Transition::at(&t, Instant::now()).expect("the transition was already over");

        let (mut floating, mut covered) = (0, 0);
        for zoom in ZOOMS {
            let camera = Camera {
                zoom,
                pan: (0.0, 0.0),
            };
            let boxes = placed(&g, Some(&frame));
            let (titles, carets) = drawn_titles(&boxes, &expandable, viewport(), camera);
            for DrawnCaret { id, at, floats } in carets {
                floating += floats as u32;
                assert_eq!(
                    crate::box_at(&titles, &g, Some(&frame), viewport(), camera, at).map(|b| b.id),
                    Some(id),
                    "zoom {zoom}: the caret a morph drew for {id:?} resolves to another box"
                );
                let (gx, gy) = from_screen(at, viewport().origin, camera);
                covered += (crate::hit_box(&g.boxes, Some(&frame), gx, gy).map(|b| b.id)
                    != Some(id)) as u32;
            }
        }
        assert!(floating > 0, "no zoom floated a title out of its band");
        assert!(
            covered > 0,
            "no caret fell on a box other than its own, so the boxes alone \
            would have resolved every gesture and the case this pins never arose"
        );
    }

    /// A status is read off the shape of its badge by a reader who cannot tell
    /// the inks apart, the three failures sharing one ink among them.
    #[test]
    fn no_two_statuses_share_a_glyph() {
        for (i, status) in Status::ALL.iter().enumerate() {
            for other in &Status::ALL[i + 1..] {
                assert_ne!(
                    Glyph::of(*status),
                    Glyph::of(*other),
                    "{status:?}/{other:?}"
                );
            }
        }
    }

    /// The box at `rect` as a frame draws it standing at `status`, where what
    /// the theme and the marked set resolve has no bearing on what is measured.
    fn badged(
        b: &bviz_graph_layout::Box,
        expandable: bool,
        status: Option<Status>,
    ) -> PaintBox<'_> {
        PaintBox {
            expandable,
            status,
            ..paint_box_of(
                &Placed {
                    b,
                    rect: Rect::of(b),
                    alpha: 1.0,
                    leaving: false,
                },
                &Expandable::Containers(Arc::new(HashSet::new())),
            )
        }
    }

    /// How far two window coordinates may part and still count as one,
    /// covering the rounding of the f32 arithmetic that places a label.
    const PLACING_SLACK: Pixels = px(0.01);

    /// How far to either side of its middle, and how far above and below it,
    /// the badge of `status` is painted at `half`: for a glyph drawn as a path,
    /// out to the corners the sides of its rim meet at.
    fn painted_reach(status: Status, half: f32) -> (f32, f32) {
        fn farthest<const N: usize>(unit: [(f32, f32); N], half: f32) -> (f32, f32) {
            outset(unit.map(|(x, y)| (x * half, y * half)), STATUS_RIM)
                .iter()
                .fold((0.0, 0.0), |(across, along), (x, y)| {
                    (x.abs().max(across), y.abs().max(along))
                })
        }
        match Glyph::of(status) {
            Glyph::TriangleUp => farthest(TRIANGLE, half),
            Glyph::TriangleDown => farthest(TRIANGLE_DOWN, half),
            Glyph::Cross => farthest(cross(), half),
            Glyph::Disc | Glyph::Bar | Glyph::Ring | Glyph::Square => {
                (half + STATUS_RIM, half + STATUS_RIM)
            }
        }
    }

    /// A badge is laid out with the label it goes before: wherever a label is
    /// drawn, what is painted of the badge lies on the label's plate and ends
    /// before the caret begins, the caret ends before the name begins, and the
    /// name ends within the plate.
    #[test]
    fn a_badge_the_caret_and_the_name_follow_one_another_on_the_plate() {
        let boxes = [
            lbox(0, "container", 0.0, 0.0, 4000.0, 3000.0, true),
            lbox(1, "leaf", 0.0, 0.0, 400.0, 30.0, false),
        ];
        let mut labelled = 0;
        for zoom in ZOOMS {
            let camera = Camera {
                zoom,
                pan: (0.0, 0.0),
            };
            for (b, expandable) in boxes.iter().flat_map(|b| [(b, false), (b, true)]) {
                for status in Status::ALL {
                    let pb = badged(b, expandable, Some(status));
                    let rect = screen_rect(&pb, viewport(), camera);
                    let Some(placement) = LabelPlacement::of(&pb, rect, zoom) else {
                        continue;
                    };
                    labelled += 1;
                    let case = format!("zoom {zoom} {:?} {status:?}", b.id);
                    let plate = placement.plate(rect, placement.avail);
                    let badge = placement.badge(plate, 1.0).expect("the box has a status");
                    let (across, along) = painted_reach(status, badge.half);
                    let follows = |before: Pixels, after: Pixels| {
                        assert!(before <= after + PLACING_SLACK, "{case}: {before} {after}");
                    };
                    follows(plate.left(), badge.center.x - px(across));
                    follows(plate.top(), badge.center.y - px(along));
                    follows(badge.center.y + px(along), plate.bottom());
                    let caret = placement.caret_at(plate).x;
                    follows(badge.center.x + px(across), caret);
                    let text = placement.text_at(plate).x;
                    follows(caret + px(placement.caret.unwrap_or(0.)), text);
                    follows(text + px(placement.avail), plate.right());
                    follows(plate.right(), rect.right());
                }
            }
        }
        assert!(labelled > 0, "no zoom drew a label");
    }

    #[test]
    fn a_box_the_build_reports_nothing_of_holds_no_room_for_a_badge() {
        let b = lbox(0, "leaf", 0.0, 0.0, 400.0, 30.0, false);
        let camera = Camera {
            zoom: 1.0,
            pan: (0.0, 0.0),
        };
        let rect = |pb: &PaintBox| screen_rect(pb, viewport(), camera);
        let unreported = badged(&b, true, None);
        let placement = LabelPlacement::of(&unreported, rect(&unreported), 1.0).expect("a label");
        let plate = placement.plate(rect(&unreported), placement.avail);
        assert!(placement.badge(plate, 1.0).is_none());
        assert_eq!(placement.caret_at(plate).x, plate.left() + px(LABEL_PAD));

        let reported = badged(&b, true, Some(Status::Succeeded));
        let badged = LabelPlacement::of(&reported, rect(&reported), 1.0).expect("a label");
        assert!(badged.avail < placement.avail);
    }

    /// A box drawn too small for a label still says how the build ended for
    /// it: a container in the corner its title band starts at, and a leaf at
    /// its middle or, where it has the room, in the corner its bottom edge
    /// starts at.
    #[test]
    fn a_box_too_small_for_its_label_carries_its_badge_bare() {
        let zoom = 0.05;
        let camera = Camera {
            zoom,
            pan: (0.0, 0.0),
        };
        let bare = |b: &bviz_graph_layout::Box, status| {
            let pb = badged(b, false, status);
            let rect = screen_rect(&pb, viewport(), camera);
            assert!(LabelPlacement::of(&pb, rect, zoom).is_none());
            (rect, pb.bare_badge(rect, zoom))
        };
        let within = |rect: Bounds<Pixels>, bounds: Bounds<Pixels>| {
            bounds.left() >= rect.left() - PLACING_SLACK
                && bounds.top() >= rect.top() - PLACING_SLACK
                && bounds.right() <= rect.right() + PLACING_SLACK
                && bounds.bottom() <= rect.bottom() + PLACING_SLACK
        };

        let container = lbox(0, "box", 0.0, 0.0, 400.0, 280.0, true);
        let (rect, found) = bare(&container, Some(Status::RootCause));
        let found = found.expect("the box has a status");
        assert_eq!(found.badge.half, STATUS_FLOOR);
        let off = found.badge.bounds().origin - rect.origin;
        assert!(off.x.abs() < PLACING_SLACK && off.y.abs() < PLACING_SLACK);
        assert!(found.otherwise.is_none());

        let leaf = lbox(0, "box", 0.0, 0.0, 400.0, 280.0, false);
        let (rect, found) = bare(&leaf, Some(Status::RootCause));
        let found = found.expect("the box has a status");
        assert_eq!(found.badge.center, rect.center());
        let corner = StatusBadge {
            center: found.otherwise.expect("room for the badge in a corner"),
            ..found.badge
        };
        assert!(within(rect, corner.bounds()));
        assert!(corner.bounds().left() < rect.left() + PLACING_SLACK);
        assert!(corner.bounds().bottom() > rect.bottom() - PLACING_SLACK);

        let cramped = lbox(0, "box", 0.0, 0.0, 400.0, 200.0, false);
        let (rect, found) = bare(&cramped, Some(Status::RootCause));
        let found = found.expect("the box has a status");
        assert_eq!(found.badge.center, rect.center());
        assert!(found.otherwise.is_none());

        assert!(bare(&leaf, None).1.is_none());
    }

    /// A bounds of the extent a badge held at its floor has, with its top left
    /// corner at `(x, y)`.
    fn badge_bounds(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(BADGE_CELL), px(BADGE_CELL)))
    }

    /// Whether two badges meet is a matter of their bounds alone, wherever
    /// the two fall among the cells they are found by.
    #[test]
    fn a_badge_meets_another_only_where_their_bounds_do() {
        const STEPS: u8 = 44;
        for step in 0..STEPS {
            let at = 100.0 + 2.0 * BADGE_CELL * f32::from(step) / f32::from(STEPS);
            let mut ground = BadgeGround::with_capacity(1);
            ground.take(badge_bounds(at, at));
            for cells in [1.05, 1.25, 1.5, 1.75, 1.95, 3.0] {
                let apart = cells * BADGE_CELL;
                for (x, y) in [(apart, 0.0), (0.0, apart), (-apart, 0.0), (0.0, -apart)] {
                    assert!(
                        ground.is_free(badge_bounds(at + x, at + y)),
                        "at {at}, apart by {x}, {y}"
                    );
                }
            }
            for cells in [0.0, 0.5, 0.95] {
                let apart = cells * BADGE_CELL;
                for (x, y) in [(apart, 0.0), (0.0, apart), (-apart, -apart)] {
                    assert!(
                        !ground.is_free(badge_bounds(at + x, at + y)),
                        "at {at}, apart by {x}, {y}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_badge_meets_any_of_several_reaching_one_cell() {
        // Three badges clear of one another, each reaching the cell between
        // them.
        let standing = [(95.0, 95.0), (107.0, 95.0), (95.0, 107.0)];
        let mut ground = BadgeGround::with_capacity(standing.len());
        for (x, y) in standing {
            assert!(ground.is_free(badge_bounds(x, y)));
            ground.take(badge_bounds(x, y));
        }
        for (x, y) in standing {
            assert!(!ground.is_free(badge_bounds(x + 0.5, y + 0.5)));
        }
        assert!(ground.is_free(badge_bounds(107.0, 107.0)));
    }

    fn bare_at(x: f32, y: f32, status: Status) -> BareBadge {
        BareBadge {
            badge: StatusBadge {
                center: point(px(x), px(y)),
                half: STATUS_FLOOR,
                status,
                alpha: 1.0,
            },
            otherwise: None,
        }
    }

    fn placed_at(placed: &[StatusBadge]) -> Vec<(f32, f32, Status)> {
        placed
            .iter()
            .map(|badge| (badge.center.x.into(), badge.center.y.into(), badge.status))
            .collect()
    }

    #[test]
    fn of_bare_badges_on_one_ground_the_most_severe_stands() {
        let mut placed = Vec::new();
        place_bare(
            vec![
                bare_at(100.0, 100.0, Status::Succeeded),
                bare_at(105.0, 100.0, Status::RootCause),
                bare_at(110.0, 100.0, Status::Failed),
                bare_at(300.0, 100.0, Status::Succeeded),
            ],
            &[],
            &mut placed,
        );
        assert_eq!(
            placed_at(&placed),
            [
                (105.0, 100.0, Status::RootCause),
                (300.0, 100.0, Status::Succeeded)
            ]
        );
    }

    #[test]
    fn of_bare_badges_as_severe_on_one_ground_the_first_walked_stands() {
        let mut placed = Vec::new();
        place_bare(
            vec![
                bare_at(100.0, 100.0, Status::Failed),
                bare_at(105.0, 100.0, Status::Failed),
            ],
            &[],
            &mut placed,
        );
        assert_eq!(placed_at(&placed), [(100.0, 100.0, Status::Failed)]);
    }

    #[test]
    fn a_bare_badge_gives_way_to_a_badge_on_its_label_and_to_a_floated_title() {
        let labelled = bare_at(100.0, 100.0, Status::Succeeded).badge;
        let title = Bounds::new(point(px(190.0), px(90.0)), size(px(40.0), px(17.0)));
        let mut placed = vec![labelled];
        place_bare(
            vec![
                bare_at(104.0, 104.0, Status::RootCause),
                bare_at(200.0, 100.0, Status::RootCause),
                bare_at(300.0, 100.0, Status::RootCause),
            ],
            &[title],
            &mut placed,
        );
        assert_eq!(
            placed_at(&placed),
            [
                (100.0, 100.0, Status::Succeeded),
                (300.0, 100.0, Status::RootCause)
            ]
        );
    }

    #[test]
    fn a_bare_badge_whose_ground_is_taken_stands_at_its_other_place() {
        let title = Bounds::new(point(px(90.0), px(90.0)), size(px(40.0), px(17.0)));
        let elsewhere = |x, y| BareBadge {
            otherwise: Some(point(px(x), px(y))),
            ..bare_at(100.0, 100.0, Status::Succeeded)
        };
        let mut placed = Vec::new();
        place_bare(
            vec![elsewhere(100.0, 120.0), elsewhere(110.0, 100.0)],
            &[title],
            &mut placed,
        );
        assert_eq!(placed_at(&placed), [(100.0, 120.0, Status::Succeeded)]);

        let mut unhindered = Vec::new();
        place_bare(vec![elsewhere(100.0, 120.0)], &[], &mut unhindered);
        assert_eq!(placed_at(&unhindered), [(100.0, 100.0, Status::Succeeded)]);
    }

    /// The corners of the glyph `status` is drawn as, for a half-width of
    /// one; `None` for a round one.
    fn glyph_corners(status: Status) -> Option<Vec<(f32, f32)>> {
        let block = |w: f32, h: f32| vec![(-w, -h), (w, -h), (w, h), (-w, h)];
        match Glyph::of(status) {
            Glyph::Disc | Glyph::Ring => None,
            Glyph::Bar => Some(block(1.0, BAR_HALF_HEIGHT)),
            Glyph::Square => Some(block(SQUARE_HALF_SIDE, SQUARE_HALF_SIDE)),
            Glyph::TriangleUp => Some(TRIANGLE.to_vec()),
            Glyph::TriangleDown => Some(TRIANGLE_DOWN.to_vec()),
            Glyph::Cross => Some(cross().to_vec()),
        }
    }

    #[test]
    fn no_status_glyph_is_the_diamond_of_a_transition_badge() {
        for status in Status::ALL {
            let Some(corners) = glyph_corners(status) else {
                continue;
            };
            let diamond = corners.len() == DIAMOND.len()
                && DIAMOND.iter().all(|(x, y)| {
                    corners.iter().any(|(cx, cy)| {
                        (cx - x).abs() < OUTLINE_SLACK && (cy - y).abs() < OUTLINE_SLACK
                    })
                });
            assert!(!diamond, "{status:?}");
        }
    }

    /// How far two coordinates may part and still count as one, covering the
    /// rounding of the f32 arithmetic an outline goes through.
    const OUTLINE_SLACK: f32 = 1e-4;

    fn assert_outline(found: &[(f32, f32)], expected: &[(f32, f32)]) {
        assert_eq!(found.len(), expected.len());
        for (found, expected) in found.iter().zip(expected) {
            assert!(
                (found.0 - expected.0).abs() < OUTLINE_SLACK
                    && (found.1 - expected.1).abs() < OUTLINE_SLACK,
                "{found:?} for {expected:?}"
            );
        }
    }

    /// A rim stands the same width off every side of its glyph, round a corner
    /// turning either way and whichever way round the corners are listed.
    #[test]
    fn an_outline_set_out_keeps_one_distance_from_every_side() {
        let square = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)];
        let grown = [(-1.5, -1.5), (1.5, -1.5), (1.5, 1.5), (-1.5, 1.5)];
        assert_outline(&outset(square, 0.5), &grown);
        let (mut reversed, mut grown_reversed) = (square, grown);
        reversed.reverse();
        grown_reversed.reverse();
        assert_outline(&outset(reversed, 0.5), &grown_reversed);

        // An L, whose one corner turns the other way from the rest.
        let l = [
            (0.0, 0.0),
            (2.0, 0.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (0.0, 2.0),
        ];
        let grown = [
            (-0.25, -0.25),
            (2.25, -0.25),
            (2.25, 1.25),
            (1.25, 1.25),
            (1.25, 2.25),
            (-0.25, 2.25),
        ];
        assert_outline(&outset(l, 0.25), &grown);
    }

    #[test]
    fn a_box_on_its_way_out_carries_no_disclosure_caret() {
        let g = covered_container();
        let expandable = Expandable::Containers(Arc::new(HashSet::from([Id(0)])));
        let b = &g.boxes[0];
        let held = |leaving| Placed {
            b,
            rect: Rect::of(b),
            alpha: 1.0,
            leaving,
        };
        assert!(
            held(false).expandable(&expandable),
            "the container the layout opens and closes carries no caret",
        );
        assert!(
            !held(true).expandable(&expandable),
            "a container the relayout dropped still carries a caret it cannot answer",
        );
    }

    /// The width of each container in [`side_by_side`], also the distance a
    /// relayout has to slide them for one to take the other's ground.
    const CONTAINER_WIDTH: f64 = 2000.0;

    /// Two containers abreast, each carrying a title of its own.
    fn side_by_side() -> LaidOut {
        let (w, h) = (CONTAINER_WIDTH, 1200.0);
        LaidOut {
            boxes: vec![
                lbox(0, "left", 0.0, 0.0, w, h, true),
                lbox(1, "right", w, 0.0, w, h, true),
            ],
            ..Default::default()
        }
    }

    /// The same boxes laid out `dx` units along, keeping every id.
    fn slid(g: &LaidOut, dx: f64) -> LaidOut {
        LaidOut {
            boxes: g
                .boxes
                .iter()
                .map(|b| {
                    lbox(
                        b.id.0,
                        &b.label,
                        b.x + dx,
                        b.y,
                        b.width,
                        b.height,
                        b.compound,
                    )
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_title_the_layout_has_moved_out_from_under_resolves_to_the_box_now_there() {
        let g = side_by_side();
        let expandable = Expandable::Containers(Arc::new(HashSet::from([Id(0), Id(1)])));
        let camera = Camera {
            zoom: 0.2,
            pan: (0.0, 0.0),
        };
        let (titles, carets) = drawn_titles(&placed(&g, None), &expandable, viewport(), camera);
        let left = carets.iter().find(|c| c.id == Id(0));
        let at = left.expect("the left container drew no title").at;
        assert_eq!(
            crate::box_at(&titles, &g, None, viewport(), camera, at).map(|b| b.id),
            Some(Id(0))
        );

        // A relayout that keeps both ids and slides the right container onto the
        // ground the left one's title was painted over. The recorded plate still
        // covers the cursor and still names a box the layout holds; what it no
        // longer names is the box drawn there.
        let moved = slid(&g, -CONTAINER_WIDTH);
        let (gx, gy) = from_screen(at, viewport().origin, camera);
        assert_eq!(
            crate::hit_box(&moved.boxes, None, gx, gy).map(|b| b.id),
            Some(Id(1)),
            "the slide left the caret over no other container, so nothing was displaced"
        );
        assert_eq!(
            crate::box_at(&titles, &moved, None, viewport(), camera, at).map(|b| b.id),
            Some(Id(1)),
            "a title from a frame the layout has moved on from still answered for it"
        );
    }
}

#[cfg(test)]
mod colors {
    use super::*;
    use bviz_ui_theme::{
        MIN_DISTINCT_COLOR, MIN_EDGED_STEP, MIN_SECONDARY_CONTRAST, MIN_TEXT_CONTRAST,
        MIN_VISIBLE_STEP, built_in_themes, contrast_ratio, distance, label_on, level_step,
        luminance, oklch, over,
    };
    use gpui::SharedString;

    /// One node kind per accent slot: the three that carry a slot of their own,
    /// and one standing for every kind that takes the catch-all.
    const ACCENTED_KINDS: [Kind; 4] = [Kind::Action, Kind::Artifact, Kind::Source, Kind::Target];

    /// One node kind per container fill: the two that carry a fill of their own,
    /// and one standing for every kind that takes the plain one.
    const CONTAINER_KINDS: [Kind; 3] = [Kind::Package, Kind::MergeGroup, Kind::Target];

    /// One node kind per container fill a container holding actions wears.
    const TIMED_CONTAINER_KINDS: [Kind; 2] = [Kind::Package, Kind::Target];

    /// Every provenance a connector is drawn in.
    const EDGE_KINDS: [EdgeKind; 4] = [
        EdgeKind::Query,
        EdgeKind::Cquery,
        EdgeKind::Aquery,
        EdgeKind::Transitive,
    ];

    /// Every theme's palette, under the name it answers to, for a rule that has
    /// to hold whichever one a window paints with.
    fn palettes() -> Vec<(SharedString, Palette)> {
        built_in_themes()
            .iter()
            .map(|theme| (theme.name.clone(), Palette::from_theme(theme)))
            .collect()
    }

    /// Places along the time ramp standing for every fill it gives: its two
    /// ends and evenly between.
    const RAMP_PLACES: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

    /// Every fill a box is painted in, each once.
    fn box_fills(p: &Palette) -> Vec<Rgba> {
        ACCENTED_KINDS
            .iter()
            .map(|kind| p.leaf_fill(*kind))
            .chain(CONTAINER_KINDS.iter().map(|kind| p.container_fill(*kind)))
            .chain([p.untimed_fill(Kind::Action, false)])
            .chain(readings(p).into_iter().flat_map(|fill| {
                TIMED_CONTAINER_KINDS
                    .map(|kind| p.held_fill(kind, true, fill))
                    .into_iter()
                    .chain([fill])
            }))
            .collect()
    }

    /// Every fill a box coloured by runner wears.
    fn ran_fills(p: &Palette) -> Vec<Rgba> {
        Runner::ALL
            .map(Ran::On)
            .into_iter()
            .chain([Ran::Mixed])
            .map(|ran| p.ran_fill(ran))
            .collect()
    }

    /// Every fill a closed box coloured by playback wears.
    fn state_fills(p: &Palette) -> Vec<Rgba> {
        NodeState::ALL.map(|state| p.state_fill(state)).into()
    }

    /// Every fill a closed box coloured by time, by runner or by playback
    /// wears.
    fn readings(p: &Palette) -> Vec<Rgba> {
        RAMP_PLACES
            .map(|place| p.time_fill(place))
            .into_iter()
            .chain(ran_fills(p))
            .chain(state_fills(p))
            .collect()
    }

    /// Every ground a connector is drawn over, each once. A marked connector runs
    /// between marked boxes, and the boxes are painted before the connectors, so
    /// each fill counts both bare and under the hover mark's wash.
    fn edge_grounds(p: &Palette) -> Vec<Rgba> {
        box_fills(p)
            .into_iter()
            .flat_map(|fill| [fill, over(p.highlight_wash(1.0), fill)])
            .chain([p.background])
            .collect()
    }

    /// Every ink a route is drawn beside, each once: the text the controls
    /// framing the canvas are set in, and the name a box carries over the routes
    /// crossing it.
    fn edge_inks(p: &Palette) -> Vec<Rgba> {
        let mut inks = vec![p.label];
        for fill in box_fills(p) {
            let ink = label_on(fill).into();
            if !inks.contains(&ink) {
                inks.push(ink);
            }
        }
        inks
    }

    /// A badge's kind is read off its fill, so no two kinds may be taken for
    /// each other, for the stroke of a route, or for the canvas rim around it.
    #[test]
    fn every_transition_badge_is_told_from_the_others_and_from_every_route() {
        for (theme, p) in palettes() {
            for (i, kind) in Transition::ALL.iter().enumerate() {
                let fill = p.transition_color(*kind);
                let rim = distance(fill, p.background);
                assert!(rim >= MIN_DISTINCT_COLOR, "{theme} {kind:?}: {rim}");
                for edge in EDGE_KINDS {
                    let apart = distance(fill, p.edge_color(edge, false));
                    assert!(
                        apart >= MIN_DISTINCT_COLOR,
                        "{theme} {kind:?}/{edge:?}: {apart}"
                    );
                }
                for other in &Transition::ALL[i + 1..] {
                    let apart = distance(fill, p.transition_color(*other));
                    assert!(
                        apart >= MIN_DISTINCT_COLOR,
                        "{theme} {kind:?}/{other:?}: {apart}"
                    );
                }
            }
        }
    }

    /// An untimed box must not pass for one holding the least time of its
    /// kind, and the ramp must stand further off the canvas at every step
    /// along it.
    #[test]
    fn the_time_ramp_is_told_from_an_untimed_box_and_steps_off_the_canvas() {
        for (theme, p) in palettes() {
            let least = p.time_fill(0.0);
            for untimed in [
                p.untimed_fill(Kind::Action, false),
                p.untimed_fill(Kind::Target, true),
                p.untimed_fill(Kind::Package, true),
            ] {
                let apart = distance(least, untimed);
                assert!(apart >= MIN_DISTINCT_COLOR, "{theme}: {apart}");
            }
            let canvas = luminance(p.background);
            let off_canvas: Vec<f32> = RAMP_PLACES
                .iter()
                .map(|place| (luminance(p.time_fill(*place)) - canvas).abs())
                .collect();
            assert!(off_canvas.is_sorted(), "{theme}: {off_canvas:?}");
        }
    }

    /// A box held in an open container that reads the same as the container
    /// must still stand off it, or the container swallows it.
    #[test]
    fn a_box_stands_off_the_open_container_reading_the_same() {
        for (theme, p) in palettes() {
            for fill in readings(&p) {
                for kind in TIMED_CONTAINER_KINDS {
                    let open = p.held_fill(kind, true, fill);
                    let apart = distance(open, fill);
                    assert!(apart >= MIN_VISIBLE_STEP, "{theme} {kind:?}: {apart}");
                    for ground in [open, over(p.highlight_wash(1.0), open)] {
                        let held = contrast_ratio(label_on(open), ground);
                        assert!(held >= MIN_TEXT_CONTRAST, "{theme} {kind:?}: {held}");
                    }
                }
            }
        }
    }

    /// A box's runner is read off its fill, so no two runners, nor a mix of
    /// them, may be taken for each other or for a box no action lands on.
    #[test]
    fn every_runner_is_told_from_the_others_and_from_an_untimed_box() {
        for (theme, p) in palettes() {
            let fills = ran_fills(&p);
            for (i, fill) in fills.iter().enumerate() {
                for untimed in [
                    p.untimed_fill(Kind::Action, false),
                    p.untimed_fill(Kind::Target, true),
                    p.untimed_fill(Kind::Package, true),
                    p.background,
                ] {
                    let apart = distance(*fill, untimed);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{theme} {i}: {apart}");
                }
                for (j, other) in fills.iter().enumerate().skip(i + 1) {
                    let apart = distance(*fill, *other);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{theme} {i}/{j}: {apart}");
                }
            }
        }
    }

    /// A box further along reads as further along: each stage its attempts
    /// enter is deeper in chroma than the one before it, so running stands
    /// out most, and an ended attempt is not as deep as one waiting on its
    /// turn to run.
    #[test]
    fn an_attempt_deepens_in_chroma_as_it_moves_on_and_settles_once_ended() {
        let running = [
            Running::Container,
            Running::Action(Venue::Local),
            Running::Action(Venue::Remote),
        ]
        .map(Stage::Running);
        for (theme, p) in palettes() {
            let chroma = |state| oklch(p.state_fill(state)).chroma;
            let under_way = [Stage::Preparing, Stage::CheckingCache, Stage::Scheduling];
            let mut before = chroma(NodeState::Pending);
            for stage in under_way {
                let deeper = chroma(NodeState::In(stage));
                assert!(
                    deeper > before,
                    "{theme} {stage:?}: {deeper} after {before}"
                );
                before = deeper;
            }
            for stage in running {
                let deeper = chroma(NodeState::In(stage));
                assert!(
                    deeper > before,
                    "{theme} {stage:?}: {deeper} after {before}"
                );
            }
            let scheduling = chroma(NodeState::In(Stage::Scheduling));
            for settled in [
                NodeState::Done(Done::Executed),
                NodeState::Done(Done::Cached),
            ] {
                let settles = chroma(settled);
                assert!(
                    settles < scheduling,
                    "{theme} {settled:?}: {settles} against {scheduling}"
                );
            }
        }
    }

    /// A box's state is read off its fill, so no two states may be taken for
    /// each other or for a box no attempt lands on.
    #[test]
    fn every_state_is_told_from_the_others_and_from_a_box_no_attempt_lands_on() {
        for (theme, p) in palettes() {
            let fills = state_fills(&p);
            for (state, fill) in NodeState::ALL.iter().zip(&fills) {
                for untimed in [
                    p.untimed_fill(Kind::Action, false),
                    p.untimed_fill(Kind::Target, true),
                    p.untimed_fill(Kind::Package, true),
                ] {
                    let apart = distance(*fill, untimed);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{theme} {state:?}: {apart}");
                }
            }
            for (i, (state, fill)) in NodeState::ALL.iter().zip(&fills).enumerate() {
                for (other, other_fill) in NodeState::ALL.iter().zip(&fills).skip(i + 1) {
                    let apart = distance(*fill, *other_fill);
                    assert!(
                        apart >= MIN_DISTINCT_COLOR,
                        "{theme} {state:?}/{other:?}: {apart}"
                    );
                }
            }
        }
    }

    /// A point's runner is read off its mark, so neither a mix of runners nor
    /// no time at all may be taken for each other or for any runner.
    #[test]
    fn a_mixed_and_an_untimed_mark_are_told_from_each_other_and_every_runner() {
        for (theme, p) in palettes() {
            let runners = Runner::ALL.map(|runner| p.ran_mark(Some(Ran::On(runner))));
            let (mixed, untimed) = (p.ran_mark(Some(Ran::Mixed)), p.ran_mark(None));
            let apart = distance(mixed, untimed);
            assert!(apart >= MIN_DISTINCT_COLOR, "{theme}: {apart}");
            for (runner, mark) in Runner::ALL.iter().zip(runners) {
                for other in [mixed, untimed] {
                    let apart = distance(mark, other);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{theme} {runner:?}: {apart}");
                }
            }
        }
    }

    #[test]
    fn every_status_ink_is_seen_on_its_rim() {
        for (theme, p) in palettes() {
            for status in Status::ALL {
                let held = contrast_ratio(p.status_color(status), p.background);
                assert!(
                    held >= MIN_SECONDARY_CONTRAST,
                    "{theme} {status:?}: {held}:1"
                );
            }
        }
    }

    /// The rim is a few pixels wide, so what lies past it counts too: on every
    /// fill a badge is set on, bare or under the hover mark's wash, either the
    /// ink holds its contrast against the fill itself or the rim stands off
    /// the fill as a line of its own around the glyph.
    #[test]
    fn every_status_badge_is_seen_on_the_box_it_marks() {
        for (theme, p) in palettes() {
            for fill in box_fills(&p) {
                for ground in [fill, over(p.highlight_wash(1.0), fill)] {
                    let rimmed = distance(p.background, ground) >= MIN_DISTINCT_COLOR;
                    for status in Status::ALL {
                        let held = contrast_ratio(p.status_color(status), ground);
                        assert!(
                            rimmed || held >= MIN_SECONDARY_CONTRAST,
                            "{theme} {status:?}: {held}:1 on a fill the rim does not show on"
                        );
                    }
                }
            }
        }
    }

    /// Statuses drawn in one ink are told apart by their glyphs; the inks
    /// themselves say which kind of ending a status is, so no two of them may
    /// be taken for each other.
    #[test]
    fn the_status_inks_are_told_apart() {
        for (theme, p) in palettes() {
            let mut inks: Vec<Rgba> = Vec::with_capacity(Status::ALL.len());
            for status in Status::ALL {
                let ink = p.status_color(status);
                if !inks.contains(&ink) {
                    inks.push(ink);
                }
            }
            assert!(inks.len() > 1, "{theme}: every status wears one ink");
            for (i, ink) in inks.iter().enumerate() {
                for other in &inks[i + 1..] {
                    let apart = distance(*ink, *other);
                    assert!(apart >= MIN_DISTINCT_COLOR, "{theme}: {apart}");
                }
            }
        }
    }

    /// A leaf's kind is read off the color of the box, so no two kinds may be
    /// taken for each other and none may sink into the canvas they are drawn on.
    #[test]
    fn every_node_kind_is_told_from_the_others_and_from_the_canvas() {
        for (theme, p) in palettes() {
            for (i, kind) in ACCENTED_KINDS.iter().enumerate() {
                let fill = p.leaf_fill(*kind);
                let over_canvas = distance(fill, p.background);
                assert!(
                    over_canvas >= MIN_DISTINCT_COLOR,
                    "{theme} {kind:?}: {over_canvas}"
                );
                for other in &ACCENTED_KINDS[i + 1..] {
                    let apart = distance(fill, p.leaf_fill(*other));
                    assert!(
                        apart >= MIN_DISTINCT_COLOR,
                        "{theme} {kind:?}/{other:?}: {apart}"
                    );
                }
            }
        }
    }

    /// A connector is followed across the canvas and over the boxes it runs
    /// between, so its provenance has to be told from every ground it crosses,
    /// wherever the hover leaves it.
    #[test]
    fn every_edge_provenance_is_told_from_what_it_crosses() {
        for (theme, p) in palettes() {
            let grounds = edge_grounds(&p);
            for kind in EDGE_KINDS {
                for marked in [false, true] {
                    let color = p.edge_color(kind, marked);
                    for ground in &grounds {
                        let apart = distance(color, *ground);
                        assert!(
                            apart >= MIN_DISTINCT_COLOR,
                            "{theme} {kind:?} marked {marked} on a ground: {apart}"
                        );
                    }
                }
            }
        }
    }

    /// A route is followed past writing as well as over fills — a box's own name
    /// over the fill it crosses, the legend and toolbar text beyond the canvas.
    /// A line that meets one of those has to go on reading as a line rather than
    /// as more of the writing, wherever the hover leaves it.
    #[test]
    fn every_edge_provenance_is_told_from_the_inks_it_runs_beside() {
        for (theme, p) in palettes() {
            let inks = edge_inks(&p);
            for kind in EDGE_KINDS {
                for marked in [false, true] {
                    let color = p.edge_color(kind, marked);
                    for ink in &inks {
                        let apart = distance(color, *ink);
                        assert!(
                            apart >= MIN_DISTINCT_COLOR,
                            "{theme} {kind:?} marked {marked} beside an ink: {apart}"
                        );
                    }
                }
            }
        }
    }

    /// The color of a connector names the query it came from, and a lane runs
    /// beside lanes of every other provenance, each of the pair left as the
    /// hover found it or carried by [`MARK_LIFT`]. No two of those meet.
    #[test]
    fn no_two_provenances_meet_however_the_hover_marks_them() {
        for (theme, p) in palettes() {
            for (i, kind) in EDGE_KINDS.iter().enumerate() {
                for other in &EDGE_KINDS[i + 1..] {
                    for marked in [false, true] {
                        for other_marked in [false, true] {
                            let apart = distance(
                                p.edge_color(*kind, marked),
                                p.edge_color(*other, other_marked),
                            );
                            assert!(
                                apart >= MIN_DISTINCT_COLOR,
                                "{theme} {kind:?} marked {marked} / {other:?} marked \
                                 {other_marked}: {apart}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// A marked lane runs beside unmarked lanes of its own query, so the mark
    /// has to carry the line clear of the one it is drawn from.
    #[test]
    fn a_marked_connector_is_told_from_the_unmarked_line_of_its_own_query() {
        for (theme, p) in palettes() {
            for kind in EDGE_KINDS {
                let seen = distance(p.edge_color(kind, true), p.edge_color(kind, false));
                assert!(seen >= MIN_DISTINCT_COLOR, "{theme} {kind:?}: {seen}");
            }
        }
    }

    /// Slack allowed on a hue compared across the lift, as a turn of the wheel,
    /// covering the rounding of the f32 arithmetic through it.
    const HUE_SLACK: f32 = 1e-3;

    /// A connector's hue is what names the query it came from, so a mark leaves
    /// the connector saying what it said and only turns the light up on it.
    #[test]
    fn a_mark_holds_the_hue_that_names_the_query() {
        for (theme, p) in palettes() {
            for kind in EDGE_KINDS {
                let bare = oklch(p.edge_color(kind, false)).hue;
                let marked = oklch(p.edge_color(kind, true)).hue;
                let turned = (bare - marked).abs();
                assert!(
                    turned.min(1.0 - turned) < HUE_SLACK,
                    "{theme} {kind:?}: hue {bare} -> {marked}"
                );
            }
        }
    }

    /// Which way a mark carries a connector is the theme's to say: a marked line
    /// stands further off the canvas in lightness than the line it marks,
    /// whether the canvas is the dark end of the range or the light one.
    #[test]
    fn a_mark_carries_a_connector_away_from_the_canvas() {
        for (theme, p) in palettes() {
            let canvas = luminance(p.background);
            for kind in EDGE_KINDS {
                let bare = luminance(p.edge_color(kind, false)) - canvas;
                let marked = luminance(p.edge_color(kind, true)) - canvas;
                assert!(
                    marked.abs() > bare.abs() && marked.signum() == bare.signum(),
                    "{theme} {kind:?}: {bare} -> {marked} off the canvas"
                );
            }
        }
    }

    /// The accent border is what draws the eye back to the box just toggled, so
    /// it has to stand off the border it replaces, the fill it is drawn around,
    /// the canvas beyond it and the connectors meeting it.
    #[test]
    fn the_highlight_is_told_from_everything_it_is_drawn_against() {
        for (theme, p) in palettes() {
            let against = box_fills(&p)
                .into_iter()
                .chain([p.background, p.border])
                .chain(
                    EDGE_KINDS
                        .iter()
                        .flat_map(|kind| [p.edge_color(*kind, false), p.edge_color(*kind, true)]),
                );
            for color in against {
                let apart = distance(p.highlight, color);
                assert!(apart >= MIN_DISTINCT_COLOR, "{theme}: {apart}");
            }
        }
    }

    /// A box carries its name on its own fill, whichever kind picked that fill,
    /// and on the ground the hover mark's wash makes of it — the one wash a fill
    /// ever wears. The ink is picked from the fill alone, so that one choice has
    /// to hold on both.
    #[test]
    fn a_name_is_legible_on_the_box_it_names() {
        for (theme, p) in palettes() {
            for fill in box_fills(&p) {
                let ink = label_on(fill);
                for ground in [fill, over(p.highlight_wash(1.0), fill)] {
                    let held = contrast_ratio(ink, ground);
                    assert!(held >= MIN_TEXT_CONTRAST, "{theme}: {held}");
                }
            }
        }
    }

    /// Nesting is read off the fills: a container stands off the canvas it is
    /// laid on, and the package holding it stands off the container in turn.
    /// Each pair meets at a drawn border.
    #[test]
    fn a_container_and_the_package_holding_it_step_off_the_canvas_in_turn() {
        for (theme, p) in palettes() {
            let plain = p.container_fill(Kind::Target);
            let off_canvas = level_step(plain, p.background);
            assert!(off_canvas >= MIN_EDGED_STEP, "{theme}: {off_canvas} levels");
            let off_container = level_step(p.container_fill(Kind::Package), plain);
            assert!(
                off_container >= MIN_EDGED_STEP,
                "{theme}: {off_container} levels"
            );
        }
    }

    /// The zooms the stroke invariants are checked over, spanning both sides of
    /// the zoom at which the thinnest line reaches the floor.
    const STROKE_ZOOMS: [f32; 8] = [1e-4, 1e-3, 0.01, 0.1, 0.5, 0.9, 1.0, 4.0];

    /// A line thinner than a pixel is drawn or not according to where the pan
    /// leaves it, so the zoom-out that shrinks a graph to a map must not take
    /// the routes off it.
    #[test]
    fn a_connector_holds_a_pixel_of_width_however_far_the_zoom_is_out() {
        for zoom in STROKE_ZOOMS {
            let w = stroke_width(CONNECTOR_WIDTH, zoom);
            assert!(w >= px(CONNECTOR_FLOOR), "zoom {zoom}: {w:?}");
        }
    }

    /// A mark says which route it picks out by the heavier line it draws it in,
    /// which it has to go on saying at a zoom holding the thinnest line up.
    #[test]
    fn a_marked_connector_stays_the_heavier_line_at_every_zoom() {
        for zoom in STROKE_ZOOMS {
            let (marked, bare) = (
                stroke_width(MARKED_WIDTH, zoom),
                stroke_width(CONNECTOR_WIDTH, zoom),
            );
            assert!(marked > bare, "zoom {zoom}: {marked:?} over {bare:?}");
        }
    }

    /// A dash the camera has shrunk toward a pixel is kept or dropped by where
    /// the pan leaves it, so a bridge zoomed out to a map reads as a dotted line
    /// rather than flickering between one and a solid line; zoomed in, the
    /// pattern follows the camera like the route it runs along.
    #[test]
    fn a_bridge_holds_its_pattern_however_far_the_zoom_is_out() {
        for zoom in STROKE_ZOOMS {
            let (dash, gap) = bridge_pattern(zoom);
            assert!(dash >= px(BRIDGE_PATTERN_FLOOR), "zoom {zoom}: {dash:?}");
            assert!(gap >= px(BRIDGE_PATTERN_FLOOR), "zoom {zoom}: {gap:?}");
        }
        assert_eq!(
            bridge_pattern(4.0),
            (px(BRIDGE_DASH * 4.0), px(BRIDGE_GAP * 4.0))
        );
    }

    /// A box the selection leaves out and the filter fades is left out by both
    /// for the same reason, so it recedes once: taking both fades would leave it
    /// too faint to say where in the graph the marked part sits.
    #[test]
    fn a_faded_box_the_selection_also_leaves_out_recedes_no_further() {
        assert_eq!(receded(1.0, 1.0), 1.0);
        assert_eq!(receded(UNMARKED_FADE, 1.0), UNMARKED_FADE);
        assert_eq!(receded(1.0, UNMARKED_FADE), UNMARKED_FADE);
        assert_eq!(receded(UNMARKED_FADE, UNMARKED_FADE), UNMARKED_FADE);
    }

    /// The floor is what a zoom-out runs into, and nothing a line wider than it
    /// meets: above the floor the camera alone says how wide a line is drawn.
    #[test]
    fn a_line_the_zoom_keeps_above_the_floor_follows_the_camera() {
        for zoom in [0.7, 1.0, 4.0] {
            assert_eq!(
                stroke_width(CONNECTOR_WIDTH, zoom),
                px(CONNECTOR_WIDTH * zoom)
            );
        }
    }

    /// Above the floor a line is drawn at the width the camera asks for, so all
    /// of the band drawn is its own and none of the ink is taken back. The taper
    /// meets the floor at the zoom the floor begins at, leaving no step in a line
    /// as the zoom carries it across.
    #[test]
    fn a_line_the_zoom_keeps_above_the_floor_is_drawn_whole() {
        for zoom in STROKE_ZOOMS.iter().filter(|z| **z >= FLOORED_BELOW) {
            assert_eq!(ink_taper(*zoom), 1.0, "zoom {zoom}");
        }
        assert_eq!(ink_taper(FLOORED_BELOW), 1.0);
        let approaching = ink_taper(FLOORED_BELOW * (1.0 - CONTINUITY_STEP));
        assert!(
            (1.0 - approaching) <= CONTINUITY_STEP,
            "the taper steps to {approaching} as the floor is met"
        );
    }

    /// How far below [`FLOORED_BELOW`] the taper is read to check it meets the
    /// floor continuously, which bounds the step it may take there as well.
    const CONTINUITY_STEP: f32 = 1e-3;

    /// A map fits a whole graph into a window, taking the zoom as far out as the
    /// reader ever sees it; the routes drawn there are thinned but never thinned
    /// away.
    #[test]
    fn the_taper_holds_its_floor_however_far_the_zoom_is_out() {
        let zooms = STROKE_ZOOMS
            .iter()
            .copied()
            .chain([0.0, f32::MIN_POSITIVE, 1e-30]);
        for zoom in zooms {
            let taper = ink_taper(zoom);
            assert!((TAPER_FLOOR..=1.0).contains(&taper), "zoom {zoom}: {taper}");
        }
    }

    /// The thinning is what a route drawn on a map is seen through, so a line
    /// held at the floor has to stand off every ground it crosses just as the
    /// unthinned line does.
    #[test]
    fn a_connector_held_at_the_taper_floor_is_told_from_what_it_crosses() {
        for (theme, p) in palettes() {
            let grounds = edge_grounds(&p);
            for kind in EDGE_KINDS {
                for marked in [false, true] {
                    let color = faded(p.edge_color(kind, marked), TAPER_FLOOR);
                    for ground in &grounds {
                        let apart = distance(over(color, *ground), *ground);
                        assert!(
                            apart >= MIN_DISTINCT_COLOR,
                            "{theme} {kind:?} marked {marked} on a ground: {apart}"
                        );
                    }
                }
            }
        }
    }

    /// A selection says which routes the reader is following and how far the rest
    /// falls back; the zoom says how much of a drawn band is the line's own. The
    /// two answer different questions, so each takes its share of what the other
    /// leaves and a selection reads the same at every zoom.
    #[test]
    fn a_selection_stands_a_connector_off_the_graph_by_as_much_at_every_zoom() {
        let marks = Marks {
            hover: Neighborhood::none(),
            selection: Rc::new(Neighborhood {
                boxes: HashSet::from([Id(0)]),
                edges: HashSet::new(),
            }),
        };
        for zoom in STROKE_ZOOMS {
            let marked = marks.connector_alpha(true, 1.0, 1.0, zoom);
            let unmarked = marks.connector_alpha(false, 1.0, 1.0, zoom);
            assert_eq!(marked, ink_taper(zoom), "zoom {zoom}");
            assert_eq!(unmarked, marked * UNMARKED_FADE, "zoom {zoom}");
        }
    }

    /// A morph fades its routes out and back as the layout it draws them from is
    /// replaced, which the taper has to leave whole: a route at the turn is gone,
    /// not merely faint.
    #[test]
    fn a_morph_takes_its_routes_all_the_way_out_at_every_zoom() {
        let marks = Marks {
            hover: Neighborhood::none(),
            selection: Rc::new(Neighborhood::none()),
        };
        for zoom in STROKE_ZOOMS {
            assert_eq!(
                marks.connector_alpha(true, 1.0, 0.0, zoom),
                0.0,
                "zoom {zoom}"
            );
        }
    }

    /// A group is a container of files, which its fill says by carrying their
    /// accent — far enough from a plain container to be seen carrying it, and
    /// not so far that the group passes for one of the files inside it.
    #[test]
    fn a_group_reads_as_a_container_of_the_files_it_holds() {
        for (theme, p) in palettes() {
            let group = p.container_fill(Kind::MergeGroup);
            let off_plain = distance(group, p.container_fill(Kind::Target));
            assert!(off_plain >= MIN_DISTINCT_COLOR, "{theme}: {off_plain}");
            let off_file = distance(group, p.leaf_fill(Kind::Artifact));
            assert!(off_file >= MIN_DISTINCT_COLOR, "{theme}: {off_file}");
        }
    }

    /// The wash marks the box the hover picked out: whatever fill that box wears,
    /// the wash carries it far enough off the one the box wears unmarked to be
    /// seen beside the boxes still wearing it.
    #[test]
    fn a_marked_box_is_seen_whatever_fill_it_wears() {
        for (theme, p) in palettes() {
            for fill in box_fills(&p) {
                let seen = distance(over(p.highlight_wash(1.0), fill), fill);
                assert!(seen >= MIN_DISTINCT_COLOR, "{theme}: {seen}");
            }
        }
    }
}
