//! Where a panel dragged over the window lands, out of the geometry the last
//! frame laid the window's panes out at, and what marks that landing while the
//! drag is over it.

use crate::Side;
use crate::layout::{AtEdge, BESIDE_SHARE};
use crate::workspace::{DIVIDER_GRIP, Destination};
use gpui::{Axis, Bounds, Pixels, Point, Size, point, px, size};

/// Within this much of an edge of the window body, a drop puts the panel in a
/// pane spanning that whole edge rather than anywhere among the panes. Held to
/// half of the band the pane under the cursor claims on the same axis, so that
/// a pane narrower than two of these bands keeps half of its own band to be
/// aimed at; the pane's band then begins where this one ends.
const WINDOW_EDGE_BAND: Pixels = px(28.);

/// The share of a pane's extent, in from each of its edges, within which a drop
/// puts the panel in a pane of its own on that side rather than among the
/// pane's tabs.
const PANE_BAND_SHARE: f32 = 0.25;

/// The deepest a pane's side band reaches in, so that a large pane keeps a
/// large middle to drop a tab into.
const PANE_BAND_MAX: Pixels = px(120.);

/// How far beyond the window a release has to land to ask for a window of its
/// own, so that a drag flicked past the edge and back in is not a tear-out.
const TEAR_OUT_MARGIN: Pixels = px(24.);

/// How wide the caret marking where a drop joins a pane's tabs is drawn.
const CARET_WIDTH: Pixels = px(3.);

/// How far apart two edges a layout means to coincide may be laid out and still
/// be read as the same edge, so that a rule turning on one of them survives the
/// sub-pixel placement a frame reports.
const COINCIDENT: Pixels = px(1.);

/// Where the last frame laid one pane of a window out, as a drag over the
/// window reads it.
pub(crate) struct PaneGeometry<P> {
    pub pane: P,
    pub bounds: Bounds<Pixels>,
    /// How many panels the pane holds, which is where a drop on its body puts
    /// the dragged one: after all of them.
    pub tab_count: usize,
    pub tabs: Vec<Bounds<Pixels>>,
}

impl<P> PaneGeometry<P> {
    /// The band of tabs above the pane's body, which reaches across the whole
    /// pane: a cursor past the last tab is still over the band. `None` where no
    /// frame has laid a tab out.
    fn band(&self) -> Option<Bounds<Pixels>> {
        let tab = self.tabs.first()?;
        Some(Bounds::new(
            point(self.bounds.origin.x, tab.origin.y),
            size(self.bounds.size.width, tab.size.height),
        ))
    }

    /// The pane less the tabs it puts up along its top: what its own side bands
    /// are measured in from, the tabs being aimed at ahead of those bands.
    fn aim(&self) -> Bounds<Pixels> {
        below(self.bounds, self.band())
    }

    /// Where among the pane's tabs a drop at `cursor` joins them: ahead of the
    /// tab whose leading half it is over, and after the last where it is past
    /// them all. `None` where the cursor is not over the band of tabs.
    fn insertion(&self, cursor: Point<Pixels>) -> Option<usize> {
        if !self.band()?.contains(&cursor) {
            return None;
        }
        Some(
            self.tabs
                .iter()
                .position(|tab| cursor.x < tab.center().x)
                .unwrap_or(self.tabs.len()),
        )
    }

    /// The boundary between tabs a drop joining them at `index` would sit at:
    /// ahead of the tab there, or past the last where there is none. Held
    /// within the band, so that the boundary at either end of the strip is
    /// marked by a whole caret rather than half of one.
    fn caret(&self, index: usize) -> Option<Bounds<Pixels>> {
        let band = self.band()?;
        let at = match self.tabs.get(index) {
            Some(tab) => tab.left(),
            None => self.tabs.last()?.right(),
        };
        let left = (at - CARET_WIDTH * 0.5)
            .min(band.right() - CARET_WIDTH)
            .max(band.left());
        Some(Bounds::new(
            point(left, band.top()),
            size(CARET_WIDTH, band.size.height),
        ))
    }
}

/// Where a drag holding the panel that pane `from` holds over `cursor` would put
/// it: `window` is the whole window, `body` the region of it the panes are
/// arranged across, and `panes` where the last frame laid each of those panes
/// out. `re_homes` answers whether a destination would move the panel at all,
/// and one that would not resolves to nothing, so that a drag marks nothing a
/// drop would then leave as it was.
pub(crate) fn dropped_at<P: Clone + PartialEq>(
    cursor: Point<Pixels>,
    window: Bounds<Pixels>,
    body: Bounds<Pixels>,
    panes: &[PaneGeometry<P>],
    from: &P,
    re_homes: impl Fn(&Destination<P>) -> bool,
) -> Option<Destination<P>> {
    let to = landing(cursor, window, body, panes, from)?;
    re_homes(&to).then_some(to)
}

/// The zone `cursor` is in, before the move it asks for is put to `re_homes`.
/// A pane's tabs are the narrowest aim of the three and are taken first; the
/// window's edge is taken ahead of the pane's own bands, which is what puts a
/// panel across the whole width or height of the window rather than beside
/// whichever pane lies against that edge.
fn landing<P: Clone + PartialEq>(
    cursor: Point<Pixels>,
    window: Bounds<Pixels>,
    body: Bounds<Pixels>,
    panes: &[PaneGeometry<P>],
    from: &P,
) -> Option<Destination<P>> {
    if !window.dilate(TEAR_OUT_MARGIN).contains(&cursor) {
        return Some(Destination::NewWindow);
    }
    if !body.contains(&cursor) {
        return None;
    }
    let over = aimed_at(panes, cursor)?;
    let on = pinned(cursor, over.bounds);
    if let Some(index) = over.insertion(on) {
        return Some(Destination::Tab {
            pane: over.pane.clone(),
            index,
        });
    }
    let aim = over.aim();
    let bands = band_depth(aim);
    if let Some(side) = near_edge(below(body, over.band()), cursor, window_band(bands)) {
        return Some(Destination::WindowEdge(side));
    }
    let pane = over.pane.clone();
    match near_edge(aim, on, bands) {
        Some(side) => Some(Destination::Beside { pane, side }),
        // The middle of the pane the panel is already in asks for the tabs it
        // is already among; which boundary of them it takes is what the caret
        // in the strip answers, so there is nothing to aim at here.
        None if pane == *from => None,
        None => Some(Destination::Tab {
            pane,
            index: over.tab_count,
        }),
    }
}

/// The pane a drop at `cursor` is aimed at: the one laid out under it, or the
/// nearest where it falls in the gap a divider's grip leaves between panes, so
/// that the bands either side of a divider meet over it rather than leaving it
/// a zone that refuses every drop.
fn aimed_at<P>(panes: &[PaneGeometry<P>], cursor: Point<Pixels>) -> Option<&PaneGeometry<P>> {
    panes.iter().min_by(|pane, other| {
        outside(pane.bounds, cursor).total_cmp(&outside(other.bounds, cursor))
    })
}

/// `cursor` brought onto the nearest point of `bounds`, so that one in the gap
/// a divider's grip leaves reads as one against the edge of the pane it is
/// aimed at.
fn pinned(cursor: Point<Pixels>, bounds: Bounds<Pixels>) -> Point<Pixels> {
    point(
        cursor.x.clamp(bounds.left(), bounds.right()),
        cursor.y.clamp(bounds.top(), bounds.bottom()),
    )
}

/// How far `cursor` lies outside `bounds`, which is none at all within them.
fn outside(bounds: Bounds<Pixels>, cursor: Point<Pixels>) -> f32 {
    let past = |at: Pixels, start: Pixels, end: Pixels| {
        f32::from(start - at).max(f32::from(at - end)).max(0.0)
    };
    past(cursor.x, bounds.left(), bounds.right()).hypot(past(
        cursor.y,
        bounds.top(),
        bounds.bottom(),
    ))
}

/// `bounds` less the `band` of tabs laid out along its own top edge, which is
/// the region a drop still has to aim at once those tabs have taken their own.
/// `bounds` whole where the band is laid out anywhere else, or nowhere.
fn below(bounds: Bounds<Pixels>, band: Option<Bounds<Pixels>>) -> Bounds<Pixels> {
    let Some(band) = band.filter(|band| band.top() <= bounds.top() + COINCIDENT) else {
        return bounds;
    };
    Bounds::new(
        point(bounds.left(), band.bottom()),
        size(
            bounds.size.width,
            (bounds.bottom() - band.bottom()).max(px(0.)),
        ),
    )
}

/// What a drag marks while it holds a panel over `to`.
pub(crate) enum Preview {
    /// The region of the window the panel would fill.
    Region(Bounds<Pixels>),
    /// The boundary between tabs the panel would join at.
    Caret(Bounds<Pixels>),
}

/// The region a drop at `to` marks, out of the same geometry [`dropped_at`]
/// read. `at_edge` gives what the arrangement lays a pane put against one of its
/// edges out at, which is what a drop there would fill.
pub(crate) fn preview<P: PartialEq>(
    to: &Destination<P>,
    cursor: Point<Pixels>,
    body: Bounds<Pixels>,
    panes: &[PaneGeometry<P>],
    at_edge: impl Fn(Side) -> AtEdge,
) -> Option<Preview> {
    let geometry = |pane| panes.iter().find(|geometry| &geometry.pane == pane);
    match to {
        Destination::Tab { pane, index } => {
            let over = geometry(pane)?;
            match over.insertion(pinned(cursor, over.bounds)) {
                Some(_) => over.caret(*index).map(Preview::Caret),
                None => Some(Preview::Region(over.bounds)),
            }
        }
        // One divider parts the pane from the one put beside it, out of the
        // room the pane held.
        Destination::Beside { pane, side } => Some(Preview::Region(along(
            geometry(pane)?.bounds,
            *side,
            BESIDE_SHARE,
            1,
        ))),
        Destination::WindowEdge(side) => {
            let at = at_edge(*side);
            Some(Preview::Region(along(body, *side, at.share, at.dividers)))
        }
        // The panel leaves this window, so nothing in it is marked.
        Destination::NewWindow => None,
    }
}

/// The part of `bounds` along `side` of it a pane laid out there fills: `share`
/// of what is left across that side's axis once `dividers` grips have taken
/// their own extent out of it.
fn along(bounds: Bounds<Pixels>, side: Side, share: f32, dividers: usize) -> Bounds<Pixels> {
    let grips = DIVIDER_GRIP * dividers as f32;
    let taken = |extent: Pixels| (extent - grips).max(px(0.)) * share;
    let mut region = bounds;
    match side.axis() {
        Axis::Horizontal => {
            region.size.width = taken(bounds.size.width);
            if !side.leads() {
                region.origin.x = bounds.right() - region.size.width;
            }
        }
        Axis::Vertical => {
            region.size.height = taken(bounds.size.height);
            if !side.leads() {
                region.origin.y = bounds.bottom() - region.size.height;
            }
        }
    }
    region
}

/// How deep a pane's side bands reach in from the edges of `bounds`: a share of
/// the extent across each axis, capped so a large pane keeps a large middle.
fn band_depth(bounds: Bounds<Pixels>) -> Size<Pixels> {
    let depth = |extent: Pixels| (extent * PANE_BAND_SHARE).min(PANE_BAND_MAX);
    size(depth(bounds.size.width), depth(bounds.size.height))
}

/// How deep the window's edge band reaches in beside a pane whose own bands are
/// `pane` deep, per [`WINDOW_EDGE_BAND`].
fn window_band(pane: Size<Pixels>) -> Size<Pixels> {
    let depth = |band: Pixels| WINDOW_EDGE_BAND.min(band * 0.5);
    size(depth(pane.width), depth(pane.height))
}

/// Which edge of `bounds` a cursor at `cursor` is within `depth` of. Where it
/// is within two of them — near a corner — the edge it is nearer as a share of
/// the extent measured across wins, so the corner is parted along the diagonal
/// of `bounds` and a cursor either side of that line has one answer rather than
/// two it flickers between. `None` in the middle, or where `bounds` has no
/// extent to measure a share of.
fn near_edge(bounds: Bounds<Pixels>, cursor: Point<Pixels>, depth: Size<Pixels>) -> Option<Side> {
    let across = |extent: Pixels| f32::from(extent);
    let (width, height) = (across(bounds.size.width), across(bounds.size.height));
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    [
        (Side::Left, cursor.x - bounds.left(), depth.width, width),
        (Side::Right, bounds.right() - cursor.x, depth.width, width),
        (Side::Top, cursor.y - bounds.top(), depth.height, height),
        (
            Side::Bottom,
            bounds.bottom() - cursor.y,
            depth.height,
            height,
        ),
    ]
    .into_iter()
    .map(|(side, into, depth, extent)| (side, f32::from(into), f32::from(depth), extent))
    .filter(|(_, into, depth, _)| (0.0..*depth).contains(into))
    .min_by(|(_, into, _, extent), (_, against, _, other)| {
        (into / extent).total_cmp(&(against / other))
    })
    .map(|(side, ..)| side)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `width` by `height` at (`x`, `y`) of the window.
    fn at(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    const TAB_HEIGHT: f32 = 30.0;
    const TAB_WIDTH: f32 = 100.0;

    /// The pane a drag starts from in a test that is not about the pane it
    /// starts from, named so that no pane of the arrangement is it.
    const ELSEWHERE: &str = "elsewhere";

    /// A pane of `tabs` panels, laid out at `bounds` with a tab of `TAB_WIDTH`
    /// for each of them across the top of it.
    fn pane(name: &'static str, bounds: Bounds<Pixels>, tabs: usize) -> PaneGeometry<&'static str> {
        PaneGeometry {
            pane: name,
            bounds,
            tab_count: tabs,
            tabs: (0..tabs)
                .map(|at| {
                    self::at(
                        f32::from(bounds.origin.x) + at as f32 * TAB_WIDTH,
                        f32::from(bounds.origin.y),
                        TAB_WIDTH,
                        TAB_HEIGHT,
                    )
                })
                .collect(),
        }
    }

    /// The whole window, and the region of it the panes are arranged across,
    /// which the chrome insets from the window's own edges.
    fn window() -> (Bounds<Pixels>, Bounds<Pixels>) {
        (at(0.0, 0.0, 1200.0, 800.0), at(0.0, 40.0, 1200.0, 720.0))
    }

    /// Two panes side by side across the body: a wide one on the left holding
    /// three panels, and a tall narrow one on the right holding one.
    fn panes() -> Vec<PaneGeometry<&'static str>> {
        vec![
            pane("wide", at(0.0, 40.0, 900.0, 720.0), 3),
            pane("tall", at(900.0, 40.0, 300.0, 720.0), 1),
        ]
    }

    /// Where a drag out of a pane of its own at (`x`, `y`) of the window lands,
    /// with every destination accepted.
    fn dropped(x: f32, y: f32) -> Option<Destination<&'static str>> {
        dropped_from(ELSEWHERE, x, y)
    }

    /// Where a drag out of `from` at (`x`, `y`) of the window lands, with every
    /// destination accepted.
    fn dropped_from(from: &'static str, x: f32, y: f32) -> Option<Destination<&'static str>> {
        let (window, body) = window();
        dropped_at(point(px(x), px(y)), window, body, &panes(), &from, |_| true)
    }

    /// A drop within the band along the body's own edge spans that edge of the
    /// window, wherever among the panes it falls.
    #[test]
    fn a_drop_against_the_bodys_edge_spans_that_edge_of_the_window() {
        assert!(matches!(
            dropped(4.0, 400.0),
            Some(Destination::WindowEdge(Side::Left))
        ));
        assert!(matches!(
            dropped(1196.0, 400.0),
            Some(Destination::WindowEdge(Side::Right))
        ));
        assert!(matches!(
            dropped(600.0, 80.0),
            Some(Destination::WindowEdge(Side::Top))
        ));
        assert!(matches!(
            dropped(600.0, 756.0),
            Some(Destination::WindowEdge(Side::Bottom))
        ));
    }

    /// The window's edge band wins over the pane's own zones, which is what
    /// puts a panel across the whole width or height of the window rather than
    /// beside the pane that happens to lie against that edge.
    #[test]
    fn the_windows_edge_beats_the_pane_band_under_it() {
        // Deep enough into the "tall" pane to be in its left band, and within
        // the window band at the body's bottom.
        assert!(matches!(
            dropped(940.0, 750.0),
            Some(Destination::WindowEdge(Side::Bottom))
        ));
        // The same column, clear of the body's edge, is the pane's own band.
        assert!(matches!(
            dropped(940.0, 400.0),
            Some(Destination::Beside {
                pane: "tall",
                side: Side::Left
            })
        ));
    }

    /// A drop on a pane's outer band puts the panel in a pane of its own on
    /// that side; one on the middle of the pane joins the pane's tabs after the
    /// panels it already holds.
    #[test]
    fn a_pane_band_splits_the_pane_and_its_middle_joins_its_tabs() {
        assert!(matches!(
            dropped(30.0, 400.0),
            Some(Destination::Beside {
                pane: "wide",
                side: Side::Left
            })
        ));
        assert!(matches!(
            dropped(870.0, 400.0),
            Some(Destination::Beside {
                pane: "wide",
                side: Side::Right
            })
        ));
        assert!(matches!(
            dropped(450.0, 700.0),
            Some(Destination::Beside {
                pane: "wide",
                side: Side::Bottom
            })
        ));
        assert!(matches!(
            dropped(450.0, 400.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 3
            })
        ));
    }

    /// The middle of the pane a panel is already in is no destination: the
    /// panel would go back among the very tabs it is already among, and which
    /// of them it sits at is what the caret in the strip answers. Its bands and
    /// its tabs are aimed at as any other pane's are.
    #[test]
    fn the_middle_of_a_panels_own_pane_is_no_destination() {
        assert!(dropped_from("wide", 450.0, 400.0).is_none());
        assert!(matches!(
            dropped_from("wide", 30.0, 400.0),
            Some(Destination::Beside {
                pane: "wide",
                side: Side::Left
            })
        ));
        assert!(matches!(
            dropped_from("wide", 260.0, 50.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 3
            })
        ));
    }

    /// A pane's band is capped in pixels, so the wider the pane the smaller the
    /// share of it the band takes.
    #[test]
    fn a_wide_panes_band_is_capped_rather_than_a_share_of_its_width() {
        let wide = at(0.0, 0.0, 2000.0, 400.0);
        assert_eq!(band_depth(wide).width, PANE_BAND_MAX);
        let narrow = at(0.0, 0.0, 200.0, 400.0);
        assert_eq!(
            band_depth(narrow).width,
            narrow.size.width * PANE_BAND_SHARE
        );
    }

    /// A pane no wider than two window-edge bands would have its whole own band
    /// swallowed by the window's, so the window's gives way to half of it and
    /// both stay aimable — against the body's left edge and against the left of
    /// the narrow pane standing there.
    #[test]
    fn a_pane_narrower_than_the_window_band_can_still_be_split_on_that_side() {
        let (window, body) = window();
        let panes = vec![
            pane("narrow", at(0.0, 40.0, 100.0, 720.0), 1),
            pane("rest", at(100.0, 40.0, 1100.0, 720.0), 1),
        ];
        let dropped = |x: f32, y: f32| {
            dropped_at(
                point(px(x), px(y)),
                window,
                body,
                &panes,
                &ELSEWHERE,
                |_| true,
            )
        };
        assert!(matches!(
            dropped(4.0, 400.0),
            Some(Destination::WindowEdge(Side::Left))
        ));
        // Past the window's band and still within the pane's own, which the
        // unbounded window band reached all the way across.
        assert!(f32::from(band_depth(panes[0].aim()).width) < f32::from(WINDOW_EDGE_BAND));
        assert!(matches!(
            dropped(20.0, 400.0),
            Some(Destination::Beside {
                pane: "narrow",
                side: Side::Left
            })
        ));
    }

    /// The same rule across the other axis, where the pane's top band is
    /// measured from below the tabs it puts up, as the window's band along that
    /// edge is.
    #[test]
    fn a_pane_shorter_than_the_window_band_can_still_be_split_on_top() {
        let (window, body) = window();
        let panes = vec![
            pane("short", at(0.0, 40.0, 1200.0, 120.0), 1),
            pane("rest", at(0.0, 160.0, 1200.0, 600.0), 1),
        ];
        let dropped = |x: f32, y: f32| {
            dropped_at(
                point(px(x), px(y)),
                window,
                body,
                &panes,
                &ELSEWHERE,
                |_| true,
            )
        };
        assert!(matches!(
            dropped(600.0, 75.0),
            Some(Destination::WindowEdge(Side::Top))
        ));
        assert!(f32::from(band_depth(panes[0].aim()).height) < f32::from(WINDOW_EDGE_BAND));
        assert!(matches!(
            dropped(600.0, 85.0),
            Some(Destination::Beside {
                pane: "short",
                side: Side::Top
            })
        ));
    }

    /// The grip between two panes belongs to the nearer of them, so the bands
    /// either side of a divider meet over it rather than leaving a band of the
    /// window that refuses every drop.
    #[test]
    fn a_drop_on_a_divider_belongs_to_the_pane_nearest_it() {
        let (window, body) = window();
        let panes = vec![
            pane("first", at(0.0, 40.0, 600.0, 720.0), 1),
            pane("second", at(607.0, 40.0, 593.0, 720.0), 1),
        ];
        let dropped = |x: f32| {
            dropped_at(
                point(px(x), px(400.)),
                window,
                body,
                &panes,
                &ELSEWHERE,
                |_| true,
            )
        };
        assert!(matches!(
            dropped(602.0),
            Some(Destination::Beside {
                pane: "first",
                side: Side::Right
            })
        ));
        assert!(matches!(
            dropped(605.0),
            Some(Destination::Beside {
                pane: "second",
                side: Side::Left
            })
        ));
    }

    /// Near a corner the cursor lies in the bands of two edges at once, and the
    /// one nearer as a share of the extent it is measured across wins: on a
    /// wide pane the vertical edges are a smaller share of the width than the
    /// horizontal ones are of the height, so a corner leans to the side the
    /// pixel distances alone would not pick.
    #[test]
    fn a_corner_of_a_wide_pane_is_parted_along_its_diagonal() {
        let wide = at(0.0, 0.0, 1000.0, 400.0);
        let depth = band_depth(wide);
        assert_eq!(depth, size(PANE_BAND_MAX, px(100.)));
        // 110 in from the left of 1000 is nearer in share than 90 down from the
        // top of 400, though further in pixels.
        let corner = point(px(110.), px(90.));
        assert_eq!(near_edge(wide, corner, depth), Some(Side::Left));
        assert!(f32::from(corner.y) < f32::from(corner.x));
    }

    /// The same rule on a tall pane picks the other way round, which a rule
    /// measuring the bands in pixels alone could not do.
    #[test]
    fn a_corner_of_a_tall_pane_is_parted_along_its_diagonal() {
        let tall = at(0.0, 0.0, 300.0, 1200.0);
        let depth = band_depth(tall);
        assert_eq!(depth, size(px(75.), PANE_BAND_MAX));
        let corner = point(px(70.), px(110.));
        assert_eq!(near_edge(tall, corner, depth), Some(Side::Top));
        assert!(f32::from(corner.x) < f32::from(corner.y));
    }

    /// The corner of the window where the tabs of the pane laid out there, the
    /// band along the body's left edge and the band along its top all cover the
    /// same column: the tabs are aimed at ahead of both bands, and below them
    /// the two bands are parted along the diagonal of the body.
    #[test]
    fn the_corner_where_the_tabs_and_both_window_bands_meet_is_parted_in_order() {
        assert!(matches!(
            dropped(6.0, 45.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 0
            })
        ));
        // Below the tabs, 6 in from a body 1200 wide is a smaller share than 6
        // down from one 690 tall, so the left edge wins the corner.
        assert!(matches!(
            dropped(6.0, 76.0),
            Some(Destination::WindowEdge(Side::Left))
        ));
        assert!(matches!(
            dropped(20.0, 72.0),
            Some(Destination::WindowEdge(Side::Top))
        ));
    }

    /// A drop over a pane's tabs joins them ahead of the tab whose leading half
    /// it is over, and after the last tab where it is past them all — which the
    /// band reaches, spanning the pane rather than only the tabs on it.
    #[test]
    fn a_drop_over_the_tabs_joins_them_where_the_caret_stands() {
        let over_tabs = |x: f32| dropped(x, 50.0);
        assert!(matches!(
            over_tabs(40.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 0
            })
        ));
        assert!(matches!(
            over_tabs(60.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 1
            })
        ));
        assert!(matches!(
            over_tabs(260.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 3
            })
        ));
        assert!(matches!(
            over_tabs(700.0),
            Some(Destination::Tab {
                pane: "wide",
                index: 3
            })
        ));
    }

    /// The tabs of a pane at the top of the window lie right against the body's
    /// own top edge, so a drop there joins them; the band spanning the window's
    /// top edge begins where they end, and both can be aimed at.
    #[test]
    fn the_tabs_along_the_bodys_top_edge_are_aimed_at_ahead_of_that_edge() {
        assert!(matches!(
            dropped(450.0, 45.0),
            Some(Destination::Tab { pane: "wide", .. })
        ));
        assert!(matches!(
            dropped(450.0, 75.0),
            Some(Destination::WindowEdge(Side::Top))
        ));
    }

    /// A strip laid out a fraction of a pixel off the body's top edge is still
    /// the strip along that edge, so the window's top band goes on beginning
    /// where the strip ends and the tabs, that band and the pane's own band are
    /// each aimable in turn. A strip laid out well below that edge belongs to a
    /// pane standing below it, and leaves the band measured from the edge.
    #[test]
    fn the_window_band_begins_past_a_strip_laid_out_within_a_pixel_of_the_edge() {
        let (window, body) = window();
        let strip = |top: f32| pane("only", at(0.0, top, 1200.0, 720.0), 1);
        let aimed = |top: f32, y: f32| {
            dropped_at(
                point(px(600.), px(y)),
                window,
                body,
                &[strip(top)],
                &ELSEWHERE,
                |_| true,
            )
        };
        for top in [40.0, 40.4] {
            assert!(matches!(
                aimed(top, 45.0),
                Some(Destination::Tab { pane: "only", .. })
            ));
            assert!(matches!(
                aimed(top, 75.0),
                Some(Destination::WindowEdge(Side::Top))
            ));
            assert!(matches!(
                aimed(top, 105.0),
                Some(Destination::Beside {
                    pane: "only",
                    side: Side::Top
                })
            ));
        }
        assert_eq!(below(body, strip(40.0).band()).top(), px(70.));
        assert_eq!(below(body, strip(40.4).band()).top(), px(70.4));
        assert_eq!(below(body, strip(400.0).band()), body);
    }

    /// A release well past the window asks for a window of its own; one just
    /// past it does not, so a drag flicked over the edge and back is not a
    /// tear-out.
    #[test]
    fn a_release_well_past_the_window_asks_for_a_window_of_its_own() {
        assert!(matches!(
            dropped(-100.0, 400.0),
            Some(Destination::NewWindow)
        ));
        assert!(matches!(
            dropped(1400.0, 900.0),
            Some(Destination::NewWindow)
        ));
        assert!(dropped(-10.0, 400.0).is_none());
    }

    /// The chrome above and below the arrangement is no destination at all.
    #[test]
    fn a_drop_on_the_chrome_lands_nowhere() {
        assert!(dropped(600.0, 20.0).is_none());
        assert!(dropped(600.0, 780.0).is_none());
    }

    /// A window whose panes no frame has laid out yet has nowhere to aim at,
    /// and one whose pane puts up no strip is aimed at by its bands alone.
    #[test]
    fn an_arrangement_with_no_tabs_or_no_panes_is_read_without_them() {
        let (window, body) = window();
        let aimed = |panes: &[PaneGeometry<&'static str>], x: f32, y: f32| {
            dropped_at(point(px(x), px(y)), window, body, panes, &ELSEWHERE, |_| {
                true
            })
        };
        assert!(aimed(&[], 600.0, 400.0).is_none());
        let bare = vec![pane("lone", at(0.0, 40.0, 1200.0, 720.0), 0)];
        assert!(matches!(
            aimed(&bare, 600.0, 45.0),
            Some(Destination::WindowEdge(Side::Top))
        ));
        assert!(matches!(
            aimed(&bare, 600.0, 400.0),
            Some(Destination::Tab {
                pane: "lone",
                index: 0
            })
        ));
    }

    /// A pane a frame gave all but no extent is nothing but the strip naming
    /// its one panel, so every drop aimed at it joins that strip; the caret
    /// marking the boundary is drawn whole where the pane leaves it no room.
    #[test]
    fn a_pane_of_a_single_pixel_is_read_without_a_band() {
        let (window, body) = window();
        let panes = vec![pane("sliver", at(0.0, 40.0, 1.0, 1.0), 1)];
        let dropped = |x: f32, y: f32| {
            dropped_at(
                point(px(x), px(y)),
                window,
                body,
                &panes,
                &ELSEWHERE,
                |_| true,
            )
        };
        for (x, y) in [(0.5, 45.0), (0.5, 400.0), (600.0, 400.0)] {
            assert!(matches!(
                dropped(x, y),
                Some(Destination::Tab {
                    pane: "sliver",
                    index: 0 | 1
                })
            ));
        }
        let caret = panes[0].caret(0).expect("the one tab has a boundary");
        assert_eq!(caret.size.width, CARET_WIDTH);
        assert_eq!(caret.left(), px(0.));
    }

    /// A destination the move would refuse is no destination, so a drag marks
    /// nothing that a drop would leave as it was.
    #[test]
    fn a_refused_destination_resolves_to_nothing() {
        let (window, body) = window();
        let refuse_the_tall_pane = |to: &Destination<&'static str>| !matches!(to, Destination::Beside { pane, .. } | Destination::Tab { pane, .. } if *pane == "tall");
        let dropped = |x: f32, y: f32| {
            dropped_at(
                point(px(x), px(y)),
                window,
                body,
                &panes(),
                &ELSEWHERE,
                refuse_the_tall_pane,
            )
        };
        assert!(dropped(1000.0, 400.0).is_none());
        assert!(matches!(
            dropped(450.0, 400.0),
            Some(Destination::Tab { pane: "wide", .. })
        ));
    }

    /// An even share of a whole arrangement, for a preview read without a real
    /// one to ask.
    fn halved(_: Side) -> AtEdge {
        AtEdge {
            share: 0.5,
            dividers: 1,
        }
    }

    /// A drop on a pane's middle marks the whole pane; one on its band marks
    /// what the new pane is laid out at beside it; one on the body's edge marks
    /// what a pane put there is laid out at — each less the extent the divider
    /// parting it from what it is put beside takes.
    #[test]
    fn a_preview_marks_the_region_the_panel_would_fill() {
        let (_, body) = window();
        let panes = panes();
        let marked = |to, cursor| match preview(&to, cursor, body, &panes, halved) {
            Some(Preview::Region(region)) => region,
            _ => panic!("a region marks this drop"),
        };
        let middle = point(px(450.), px(400.));
        assert_eq!(
            marked(
                Destination::Tab {
                    pane: "wide",
                    index: 3
                },
                middle
            ),
            at(0.0, 40.0, 900.0, 720.0)
        );
        // Half of the 900 the pane holds, less the 7 the divider between the
        // two takes: 446.5, against the pane's own right edge.
        assert_eq!(
            marked(
                Destination::Beside {
                    pane: "wide",
                    side: Side::Right
                },
                middle
            ),
            at(453.5, 40.0, 446.5, 720.0)
        );
        // Half of the body's 720, less the one divider: 356.5.
        assert_eq!(
            marked(Destination::WindowEdge(Side::Bottom), middle),
            at(0.0, 403.5, 1200.0, 356.5)
        );
    }

    /// Every divider the arrangement lays out takes its own extent out of what
    /// the panes divide, so a mark over an arrangement of several promises less
    /// room than one over an arrangement of two.
    #[test]
    fn a_preview_over_a_split_of_several_leaves_room_for_every_divider() {
        let (_, body) = window();
        let quartered = |_: Side| AtEdge {
            share: 0.25,
            dividers: 3,
        };
        let marked = match preview(
            &Destination::WindowEdge(Side::Left),
            point(px(450.), px(400.)),
            body,
            &panes(),
            quartered,
        ) {
            Some(Preview::Region(region)) => region,
            _ => panic!("a region marks this drop"),
        };
        assert_eq!(marked, at(0.0, 40.0, (1200.0 - 3.0 * 7.0) / 4.0, 720.0));
    }

    /// A drop among a pane's tabs is marked by a caret at the boundary it would
    /// join, which stands between the tabs either side of it, past the last tab
    /// at the end, and wholly within the pane at the first.
    #[test]
    fn a_preview_over_the_tabs_marks_the_boundary_it_would_join() {
        let (_, body) = window();
        let panes = panes();
        let caret = |index, x: f32| match preview(
            &Destination::Tab {
                pane: "wide",
                index,
            },
            point(px(x), px(50.)),
            body,
            &panes,
            halved,
        ) {
            Some(Preview::Caret(caret)) => caret,
            _ => panic!("a caret marks this drop"),
        };
        assert_eq!(caret(0, 40.0).left(), px(0.));
        assert_eq!(caret(1, 160.0).center().x, px(TAB_WIDTH));
        assert_eq!(caret(3, 700.0).center().x, px(3.0 * TAB_WIDTH));
        for index in [0, 1, 3] {
            let caret = caret(index, 40.0 + index as f32 * TAB_WIDTH);
            assert_eq!(caret.size.width, CARET_WIDTH);
            assert!(f32::from(caret.left()) >= 0.0);
        }
    }

    /// What a drag marks is what a drop then does: for every cursor, the mark
    /// stands over the pane, the boundary or the edge that the same cursor
    /// resolves to.
    #[test]
    fn a_preview_marks_what_the_same_cursor_would_drop_onto() {
        let (window, body) = window();
        // The two panes of `panes` with a divider's grip between them, so that
        // the cursors below take in one standing in that gap.
        let panes = vec![
            pane("wide", at(0.0, 40.0, 896.5, 720.0), 3),
            pane("tall", at(903.5, 40.0, 296.5, 720.0), 1),
        ];
        for (x, y) in [
            (450.0, 400.0),
            (30.0, 400.0),
            (866.0, 400.0),
            (450.0, 700.0),
            (4.0, 400.0),
            (600.0, 756.0),
            (1196.0, 400.0),
            (60.0, 50.0),
            (700.0, 50.0),
            (940.0, 400.0),
            (1000.0, 50.0),
            (899.0, 400.0),
            (902.0, 400.0),
            (900.0, 50.0),
        ] {
            let cursor = point(px(x), px(y));
            let to = dropped_at(cursor, window, body, &panes, &ELSEWHERE, |_| true)
                .unwrap_or_else(|| panic!("({x}, {y}) lands somewhere"));
            let marked = preview(&to, cursor, body, &panes, halved)
                .unwrap_or_else(|| panic!("({x}, {y}) is marked"));
            match (&to, marked) {
                (Destination::Tab { pane, index }, Preview::Caret(caret)) => {
                    let over = panes
                        .iter()
                        .find(|geometry| geometry.pane == *pane)
                        .unwrap();
                    assert_eq!(over.insertion(pinned(cursor, over.bounds)), Some(*index));
                    assert!(over.band().unwrap().contains(&caret.origin));
                }
                (Destination::Tab { pane, .. }, Preview::Region(region)) => {
                    let over = panes
                        .iter()
                        .find(|geometry| geometry.pane == *pane)
                        .unwrap();
                    assert_eq!(region, over.bounds);
                }
                (Destination::Beside { pane, side }, Preview::Region(region)) => {
                    let over = panes
                        .iter()
                        .find(|geometry| geometry.pane == *pane)
                        .unwrap();
                    assert!(over.bounds.contains(&region.center()));
                    assert_eq!(
                        near_edge(over.bounds, region.center(), over.bounds.size),
                        Some(*side)
                    );
                }
                (Destination::WindowEdge(side), Preview::Region(region)) => {
                    assert!(body.contains(&region.center()));
                    assert_eq!(near_edge(body, region.center(), body.size), Some(*side));
                }
                _ => panic!("({x}, {y}) is marked unlike what it lands on"),
            }
        }
    }

    /// A panel taken into a window of its own leaves this one, so a drag out
    /// past its edge marks nothing here.
    #[test]
    fn a_drag_out_of_the_window_marks_nothing_in_it() {
        let (_, body) = window();
        assert!(
            preview(
                &Destination::<&'static str>::NewWindow,
                point(px(-100.), px(400.)),
                body,
                &panes(),
                halved,
            )
            .is_none()
        );
    }
}
