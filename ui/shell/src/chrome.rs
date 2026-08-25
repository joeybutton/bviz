use crate::{CloseWindow, Quit, Tab, TabPrev};
use bviz_ui_components::Button;
use bviz_ui_theme::{ThemeColors, mix};
use gpui::{
    AnyElement, App, Bounds, BoxShadow, CursorStyle, Decorations, Div, Entity, FocusHandle,
    HitboxBehavior, Hsla, IntoElement, MouseButton, Pixels, Point, RenderOnce, ResizeEdge, Rgba,
    SharedString, Size, Stateful, Tiling, Window, canvas, div, point, prelude::*, px,
    transparent_black,
};

/// Room left around the window for the drop shadow, and the resize-edge hit zone.
const SHADOW: Pixels = px(10.0);
const ROUNDING: Pixels = px(10.0);
const BORDER: Pixels = px(1.0);

/// The share of the theme's shadow opacity the drop shadow is painted at while
/// the window is not the active one, so an inactive window sits shallower on the
/// desktop behind it.
const UNFOCUSED_SHADOW: f32 = 0.5;

/// How far the bands framing the canvas are pulled toward it while the window is
/// not the active one. Far enough that the dimming registers on its own, and
/// short of the canvas so a dimmed band still reads as a band.
const INACTIVE_BAND_PULL: f32 = 0.65;

fn inactive_band(colors: &ThemeColors) -> Rgba {
    mix(colors.surface, colors.background, INACTIVE_BAND_PULL)
}

/// The application window frame: client-side decorations (title bar with window
/// controls, status bar, resize edges, rounded corners, drop shadow, focus
/// dimming) wrapping an app-provided body. Falls back to the compositor's chrome
/// when it provides server-side decorations.
#[derive(IntoElement)]
pub struct Chrome {
    /// The root view's focus handle, so the lifecycle/tab actions dispatch here.
    focus_handle: FocusHandle,
    title: SharedString,
    status: Option<AnyElement>,
    body: Option<AnyElement>,
}

impl Chrome {
    pub fn new(focus_handle: &FocusHandle) -> Self {
        Self {
            focus_handle: focus_handle.clone(),
            title: SharedString::default(),
            status: None,
            body: None,
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    pub fn status(mut self, status: impl IntoElement) -> Self {
        self.status = Some(status.into_any_element());
        self
    }

    pub fn child(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }
}

impl RenderOnce for Chrome {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let decorations = window.window_decorations();
        let tiling = match decorations {
            Decorations::Server => Tiling::default(),
            Decorations::Client { tiling } => tiling,
        };
        let client = matches!(decorations, Decorations::Client { .. });
        window.set_client_inset(if client { SHADOW } else { px(0.) });
        let radius = if client && !tiling.is_tiled() {
            ROUNDING
        } else {
            px(0.)
        };

        let active = window.is_window_active();
        let band = if active {
            colors.surface.into()
        } else {
            inactive_band(&colors)
        };
        let shadow = if active {
            colors.shadow
        } else {
            colors.shadow.opacity(UNFOCUSED_SHADOW)
        };
        let move_latch = MoveLatch::keyed(window, cx);

        let inner = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .when(client, |this| decorate(this, &colors, tiling, shadow))
            .when(client, |this| {
                this.child(title_bar(&colors, radius, band, self.title, move_latch))
            })
            .child(self.body.unwrap_or_else(|| div().into_any_element()))
            .child(status_bar(&colors, radius, band, self.status));

        div()
            .id("window-backdrop")
            .font_family(bviz_ui_assets::UI)
            .track_focus(&self.focus_handle)
            .on_action(|_: &Quit, _, cx| cx.quit())
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &Tab, window, cx| window.focus_next(cx))
            .on_action(|_: &TabPrev, window, cx| window.focus_prev(cx))
            .size_full()
            .bg(transparent_black())
            .when(client, |this| resize_backdrop(this, tiling))
            .child(inner)
    }
}

/// The radius of one window corner, given whether either of the two edges
/// meeting there is tiled. A corner flush against the screen has nothing to
/// round off against, so it stays square.
fn corner_rounding(tiled: bool) -> Pixels {
    if tiled { px(0.) } else { ROUNDING }
}

/// Per-tiling window decoration: a border and rounded corner on each non-tiled
/// side, plus a drop shadow while the window floats free of the screen edges.
fn decorate(this: Div, colors: &ThemeColors, tiling: Tiling, shadow: Hsla) -> Div {
    this.border_color(colors.border)
        .when(!tiling.top, |d| d.border_t(BORDER))
        .when(!tiling.bottom, |d| d.border_b(BORDER))
        .when(!tiling.left, |d| d.border_l(BORDER))
        .when(!tiling.right, |d| d.border_r(BORDER))
        .rounded_tl(corner_rounding(tiling.top || tiling.left))
        .rounded_tr(corner_rounding(tiling.top || tiling.right))
        .rounded_bl(corner_rounding(tiling.bottom || tiling.left))
        .rounded_br(corner_rounding(tiling.bottom || tiling.right))
        .when(!tiling.is_tiled(), |d| {
            d.shadow(vec![BoxShadow {
                color: shadow,
                blur_radius: SHADOW / 2.,
                spread_radius: px(0.),
                offset: point(px(0.), px(0.)),
                inset: false,
            }])
        })
}

/// The resize-interaction band around the content: shadow-width padding on each
/// non-tiled side, dragging near an edge or corner starts a window resize, and a
/// canvas child updates the resize cursor there.
fn resize_backdrop(this: Stateful<Div>, tiling: Tiling) -> Stateful<Div> {
    this.when(!tiling.top, |d| d.pt(SHADOW))
        .when(!tiling.bottom, |d| d.pb(SHADOW))
        .when(!tiling.left, |d| d.pl(SHADOW))
        .when(!tiling.right, |d| d.pr(SHADOW))
        // The resize-edge cursor (set by the canvas below) only changes within
        // the border band, so repaint to update it only there. Refreshing on
        // every move would repaint the whole window — including a heavy content
        // canvas — on each motion, pinning a core while the cursor merely
        // crosses the content. The band is wider than the resize hit zone so the
        // cursor still resets once on the way out.
        .on_mouse_move(|e, window, _| {
            let size = window.window_bounds().get_bounds().size;
            let inner = Bounds::new(Point::default(), size).inset(SHADOW * 2.0);
            if !inner.contains(&e.position) {
                window.refresh();
            }
        })
        .on_mouse_down(MouseButton::Left, move |e, window, _| {
            let size = window.window_bounds().get_bounds().size;
            if let Some(edge) = resize_grip(e.position, size, SHADOW, tiling) {
                window.start_window_resize(edge);
            }
        })
        .child(resize_cursor(tiling))
}

/// A title-bar press outstanding across frames, waiting for the motion that
/// turns it into a window move. Holding the move back until the pointer actually
/// travels leaves a press the compositor takes for an edge resize — one landing
/// in a corner of the title bar — to be only that.
#[derive(Clone)]
struct MoveLatch(Entity<bool>);

impl MoveLatch {
    fn keyed(window: &mut Window, cx: &mut App) -> Self {
        Self(window.use_keyed_state("chrome-title-bar-move", cx, |_, _| false))
    }

    fn press(&self, cx: &mut App) {
        self.0.update(cx, |outstanding, _| *outstanding = true);
    }

    fn abandon(&self, cx: &mut App) {
        self.0.update(cx, |outstanding, _| *outstanding = false);
    }

    /// Whether a press is outstanding, clearing it either way: one motion is all
    /// a press gets, and a motion under no button says the press ended out of
    /// this element's sight.
    fn claim(&self, cx: &mut App) -> bool {
        self.0.update(cx, |outstanding, _| {
            let claimed = *outstanding;
            *outstanding = false;
            claimed
        })
    }
}

/// A title bar: draggable to move the window, with minimize/maximize/close controls.
fn title_bar(
    colors: &ThemeColors,
    radius: Pixels,
    band: Rgba,
    title: SharedString,
    latch: MoveLatch,
) -> impl IntoElement {
    div()
        .id("title-bar")
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .h(px(36.))
        .px_2()
        .rounded_tl(radius)
        .rounded_tr(radius)
        .bg(band)
        .border_b_1()
        .border_color(colors.border)
        .on_mouse_down(MouseButton::Left, {
            let latch = latch.clone();
            move |_, _, cx| latch.press(cx)
        })
        .on_mouse_up(MouseButton::Left, {
            let latch = latch.clone();
            move |_, _, cx| latch.abandon(cx)
        })
        // A press anywhere else belongs to a different gesture, and the drag it
        // may grow into must not be taken for the title bar's own.
        .on_mouse_down_out({
            let latch = latch.clone();
            move |_, _, cx| latch.abandon(cx)
        })
        .on_mouse_move(move |e, window, cx| {
            if latch.claim(cx) && e.dragging() {
                window.start_window_move();
            }
        })
        // Double-clicking the title bar maximizes/restores the window (like the
        // maximize control) — it fills the work area without covering the panel,
        // unlike true fullscreen.
        .on_click(|e, window, _| {
            if e.click_count() == 2 {
                window.zoom_window();
            }
        })
        .child(div().px_1().child(title))
        .child(
            div()
                .flex()
                .flex_row()
                .gap_1()
                // Don't let a button press also start a window move.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Button::new("minimize", |_, window, _| window.minimize_window())
                        .ghost()
                        .icon("minus"),
                )
                .child(
                    Button::new("maximize", |_, window, _| window.zoom_window())
                        .ghost()
                        .icon("square"),
                )
                .child(
                    Button::new("close", |_, window, _| window.remove_window())
                        .ghost()
                        .icon("x"),
                ),
        )
}

fn status_bar(
    colors: &ThemeColors,
    radius: Pixels,
    band: Rgba,
    status: Option<AnyElement>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(24.))
        .px_2()
        .rounded_bl(radius)
        .rounded_br(radius)
        .bg(band)
        .border_t_1()
        .border_color(colors.border)
        .text_color(colors.text_muted)
        .text_sm()
        .font_family(bviz_ui_assets::MONO)
        .children(status)
}

/// A behind-the-content canvas that updates the resize cursor near the edges.
fn resize_cursor(tiling: Tiling) -> impl IntoElement {
    canvas(
        |_, window, _| {
            window.insert_hitbox(
                Bounds::new(
                    point(px(0.), px(0.)),
                    window.window_bounds().get_bounds().size,
                ),
                HitboxBehavior::Normal,
            )
        },
        move |_, hitbox, window, _| {
            let size = window.window_bounds().get_bounds().size;
            let Some(edge) = resize_grip(window.mouse_position(), size, SHADOW, tiling) else {
                return;
            };
            window.set_cursor_style(
                match edge {
                    ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
                    ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
                    ResizeEdge::TopLeft | ResizeEdge::BottomRight => {
                        CursorStyle::ResizeUpLeftDownRight
                    }
                    ResizeEdge::TopRight | ResizeEdge::BottomLeft => {
                        CursorStyle::ResizeUpRightDownLeft
                    }
                },
                &hitbox,
            );
        },
    )
    .absolute()
    .size_full()
}

/// Which end of one axis a position is gripped by.
#[derive(Clone, Copy)]
enum Grip {
    Start,
    End,
}

/// Where `along` sits on an axis of length `extent`: gripped by an end when it
/// lies within `band` measured inward from that end, loose in between.
fn grip(along: Pixels, extent: Pixels, band: Pixels) -> Option<Grip> {
    if along < band {
        Some(Grip::Start)
    } else if along >= extent - band {
        Some(Grip::End)
    } else {
        None
    }
}

/// A corner grip's band as a multiple of an edge grip's. A diagonal drag is the
/// hardest of the eight to aim at, so it gets the largest target.
const CORNER_REACH: f32 = 1.5;

/// The ends of each axis a `cursor` grips within `band` of the window's edges,
/// vertical first.
fn grips(cursor: Point<Pixels>, frame: Size<Pixels>, band: Pixels) -> (Option<Grip>, Option<Grip>) {
    (
        grip(cursor.y, frame.height, band),
        grip(cursor.x, frame.width, band),
    )
}

/// The resize edge or corner a `cursor` offset from the window's top-left grips.
/// A corner is a grip on both axes and reaches `CORNER_REACH` times `band` in
/// from each of its edges; a single edge reaches `band` in from itself. An edge
/// the compositor has tiled against the screen cannot be dragged, so it gives up
/// its grip and a corner that meets it resizes from its other edge alone.
fn resize_grip(
    cursor: Point<Pixels>,
    frame: Size<Pixels>,
    band: Pixels,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let corner = grips(cursor, frame, band * CORNER_REACH);
    let (vertical, horizontal) = match corner {
        (Some(_), Some(_)) => corner,
        _ => grips(cursor, frame, band),
    };
    let free = |grip, start, end| match grip {
        Some(Grip::Start) if start => None,
        Some(Grip::End) if end => None,
        grip => grip,
    };
    match (
        free(vertical, tiling.top, tiling.bottom),
        free(horizontal, tiling.left, tiling.right),
    ) {
        (Some(Grip::Start), None) => Some(ResizeEdge::Top),
        (Some(Grip::End), None) => Some(ResizeEdge::Bottom),
        (None, Some(Grip::Start)) => Some(ResizeEdge::Left),
        (None, Some(Grip::End)) => Some(ResizeEdge::Right),
        (Some(Grip::Start), Some(Grip::Start)) => Some(ResizeEdge::TopLeft),
        (Some(Grip::Start), Some(Grip::End)) => Some(ResizeEdge::TopRight),
        (Some(Grip::End), Some(Grip::Start)) => Some(ResizeEdge::BottomLeft),
        (Some(Grip::End), Some(Grip::End)) => Some(ResizeEdge::BottomRight),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{MIN_EDGED_STEP, MIN_VISIBLE_STEP, ThemeRegistry, level_step};
    use gpui::size;

    /// Dimming the bands is what says the window is no longer the active one, and
    /// once a compositor tiles the window — taking the rounded corners and the
    /// drop shadow with them — it is the only thing saying it. So a dimmed band
    /// has to be told from a lit one, and must not sink into the canvas it frames
    /// across the border the two meet along.
    #[test]
    fn a_dimmed_band_is_told_from_a_lit_one_and_from_the_canvas() {
        let registry = ThemeRegistry::new().expect("the built-in themes load");
        for name in ["One Dark", "One Light"] {
            let colors = registry.get(name).expect("a built-in theme").colors;
            let dimmed = level_step(inactive_band(&colors), colors.surface);
            assert!(
                dimmed >= MIN_VISIBLE_STEP,
                "{name}: {dimmed} levels of dimming"
            );
            let framed = level_step(inactive_band(&colors), colors.background);
            assert!(
                framed >= MIN_EDGED_STEP,
                "{name}: {framed} levels off the canvas"
            );
        }
    }

    const FRAME_WIDTH: f32 = 400.0;
    const FRAME_HEIGHT: f32 = 300.0;
    const BAND: f32 = 8.0;
    const CORNER_BAND: f32 = BAND * CORNER_REACH;

    /// An offset that lies within the band of the start of an axis, and the
    /// offsets that lie within it of the end of each axis.
    const NEAR_START: f32 = BAND / 4.0;
    const NEAR_END_X: f32 = FRAME_WIDTH - NEAR_START;
    const NEAR_END_Y: f32 = FRAME_HEIGHT - NEAR_START;

    /// An offset a corner reaches but an edge does not.
    const CORNER_ONLY: f32 = (BAND + CORNER_BAND) / 2.0;

    const MID_X: f32 = FRAME_WIDTH / 2.0;
    const MID_Y: f32 = FRAME_HEIGHT / 2.0;

    fn tiled(top: bool, bottom: bool, left: bool, right: bool) -> Tiling {
        Tiling {
            top,
            bottom,
            left,
            right,
        }
    }

    fn grip_at(x: f32, y: f32, tiling: Tiling) -> Option<ResizeEdge> {
        resize_grip(
            point(px(x), px(y)),
            size(px(FRAME_WIDTH), px(FRAME_HEIGHT)),
            px(BAND),
            tiling,
        )
    }

    #[test]
    fn a_free_window_resizes_from_each_edge_and_each_corner() {
        let free = Tiling::default();
        assert_eq!(grip_at(MID_X, NEAR_START, free), Some(ResizeEdge::Top));
        assert_eq!(grip_at(MID_X, NEAR_END_Y, free), Some(ResizeEdge::Bottom));
        assert_eq!(grip_at(NEAR_START, MID_Y, free), Some(ResizeEdge::Left));
        assert_eq!(grip_at(NEAR_END_X, MID_Y, free), Some(ResizeEdge::Right));
        assert_eq!(
            grip_at(NEAR_START, NEAR_START, free),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(
            grip_at(NEAR_END_X, NEAR_START, free),
            Some(ResizeEdge::TopRight)
        );
        assert_eq!(
            grip_at(NEAR_START, NEAR_END_Y, free),
            Some(ResizeEdge::BottomLeft)
        );
        assert_eq!(
            grip_at(NEAR_END_X, NEAR_END_Y, free),
            Some(ResizeEdge::BottomRight)
        );
    }

    #[test]
    fn a_corner_claims_the_band_along_each_edge_that_only_it_reaches() {
        let free = Tiling::default();
        assert_eq!(
            grip_at(CORNER_ONLY, NEAR_START, free),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(
            grip_at(NEAR_START, CORNER_ONLY, free),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(
            grip_at(CORNER_BAND + 1.0, NEAR_START, free),
            Some(ResizeEdge::Top)
        );
    }

    #[test]
    fn the_interior_within_the_band_of_no_edge_does_not_resize() {
        let free = Tiling::default();
        assert_eq!(grip_at(MID_X, MID_Y, free), None);
        assert_eq!(grip_at(BAND, MID_Y, free), None);
        assert_eq!(grip_at(CORNER_BAND, CORNER_BAND, free), None);
        assert_eq!(
            grip_at(
                FRAME_WIDTH - CORNER_BAND - 1.0,
                FRAME_HEIGHT - CORNER_BAND - 1.0,
                free
            ),
            None
        );
    }

    #[test]
    fn a_tiled_edge_gives_up_its_grip_and_leaves_its_corners_the_free_one() {
        let left = tiled(false, false, true, false);
        assert_eq!(grip_at(NEAR_START, MID_Y, left), None);
        assert_eq!(grip_at(NEAR_START, NEAR_START, left), Some(ResizeEdge::Top));
        assert_eq!(
            grip_at(NEAR_START, CORNER_ONLY, left),
            Some(ResizeEdge::Top)
        );
        assert_eq!(
            grip_at(NEAR_START, NEAR_END_Y, left),
            Some(ResizeEdge::Bottom)
        );
        assert_eq!(grip_at(MID_X, NEAR_START, left), Some(ResizeEdge::Top));
        assert_eq!(
            grip_at(NEAR_END_X, NEAR_START, left),
            Some(ResizeEdge::TopRight)
        );
        assert_eq!(grip_at(NEAR_END_X, MID_Y, left), Some(ResizeEdge::Right));

        let top = tiled(true, false, false, false);
        assert_eq!(grip_at(MID_X, NEAR_START, top), None);
        assert_eq!(grip_at(NEAR_START, NEAR_START, top), Some(ResizeEdge::Left));
        assert_eq!(
            grip_at(CORNER_ONLY, NEAR_START, top),
            Some(ResizeEdge::Left)
        );
        assert_eq!(
            grip_at(NEAR_END_X, NEAR_START, top),
            Some(ResizeEdge::Right)
        );

        let bottom_right = tiled(false, true, false, true);
        assert_eq!(grip_at(NEAR_END_X, NEAR_END_Y, bottom_right), None);
        assert_eq!(
            grip_at(NEAR_START, NEAR_END_Y, bottom_right),
            Some(ResizeEdge::Left)
        );
        assert_eq!(
            grip_at(NEAR_END_X, NEAR_START, bottom_right),
            Some(ResizeEdge::Top)
        );
    }

    #[test]
    fn a_window_tiled_on_every_side_does_not_resize() {
        let all = tiled(true, true, true, true);
        for (x, y) in [
            (NEAR_START, NEAR_START),
            (MID_X, NEAR_START),
            (NEAR_END_X, NEAR_END_Y),
            (NEAR_START, MID_Y),
            (CORNER_ONLY, NEAR_START),
        ] {
            assert_eq!(grip_at(x, y, all), None, "at ({x}, {y})");
        }
    }
}
