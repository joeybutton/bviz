use crate::{CloseWindow, Quit, Tab, TabPrev};
use bviz_ui_components::Button;
use bviz_ui_theme::{ThemeColors, mix};
use gpui::{
    AnyElement, App, Bounds, BoxShadow, CursorStyle, Decorations, Div, Entity, FocusHandle,
    HitboxBehavior, Hsla, IntoElement, MouseButton, Pixels, Point, RenderOnce, ResizeEdge, Rgba,
    SharedString, Size, Stateful, Tiling, Window, canvas, div, point, prelude::*, px, size,
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
        let should_move = window.use_keyed_state("chrome-window-move", cx, |_, _| false);

        let inner = div()
            .flex()
            .flex_col()
            .size_full()
            .bg(colors.background)
            .text_color(colors.text)
            .when(client, |this| decorate(this, &colors, tiling, shadow))
            .when(client, |this| {
                this.child(title_bar(&colors, radius, band, self.title, should_move))
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

/// Per-tiling window decoration: a border and rounded corner on each non-tiled
/// side, plus a drop shadow while the window floats free of the screen edges.
fn decorate(this: Div, colors: &ThemeColors, tiling: Tiling, shadow: Hsla) -> Div {
    this.border_color(colors.border)
        .when(!tiling.top, |d| d.border_t(BORDER))
        .when(!tiling.bottom, |d| d.border_b(BORDER))
        .when(!tiling.left, |d| d.border_l(BORDER))
        .when(!tiling.right, |d| d.border_r(BORDER))
        .when(!tiling.top && !tiling.left, |d| d.rounded_tl(ROUNDING))
        .when(!tiling.top && !tiling.right, |d| d.rounded_tr(ROUNDING))
        .when(!tiling.bottom && !tiling.left, |d| d.rounded_bl(ROUNDING))
        .when(!tiling.bottom && !tiling.right, |d| d.rounded_br(ROUNDING))
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
            if let Some(edge) = resize_edge(e.position, SHADOW, size, tiling) {
                window.start_window_resize(edge);
            }
        })
        .child(resize_cursor(tiling))
}

/// A title bar: draggable to move the window, with minimize/maximize/close controls.
fn title_bar(
    colors: &ThemeColors,
    radius: Pixels,
    band: Rgba,
    title: SharedString,
    should_move: Entity<bool>,
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
        // Start the move on the first drag, not the press, so an edge resize wins.
        .on_mouse_down(MouseButton::Left, {
            let should_move = should_move.clone();
            move |_, _, cx| should_move.update(cx, |m, _| *m = true)
        })
        .on_mouse_up(MouseButton::Left, {
            let should_move = should_move.clone();
            move |_, _, cx| should_move.update(cx, |m, _| *m = false)
        })
        // A press anywhere else belongs to a different gesture, and the drag it
        // may grow into must not be taken for the title bar's own.
        .on_mouse_down_out({
            let should_move = should_move.clone();
            move |_, _, cx| should_move.update(cx, |m, _| *m = false)
        })
        // A move with no button held means the press that armed the latch is
        // over, wherever it was released, so consume it either way.
        .on_mouse_move(move |e, window, cx| {
            if should_move.update(cx, |m, _| std::mem::replace(m, false)) && e.dragging() {
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
            let Some(edge) = resize_edge(window.mouse_position(), SHADOW, size, tiling) else {
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

/// Which resize edge or corner the position is on, within `shadow_size` of the window border.
fn resize_edge(
    pos: Point<Pixels>,
    shadow_size: Pixels,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let bounds = Bounds::new(Point::default(), window_size).inset(shadow_size * 1.5);
    if bounds.contains(&pos) {
        return None;
    }

    let corner = size(shadow_size * 1.5, shadow_size * 1.5);
    if !tiling.top && Bounds::new(point(px(0.), px(0.)), corner).contains(&pos) {
        return Some(ResizeEdge::TopLeft);
    }
    if !tiling.top
        && Bounds::new(point(window_size.width - corner.width, px(0.)), corner).contains(&pos)
    {
        return Some(ResizeEdge::TopRight);
    }
    if !tiling.bottom
        && Bounds::new(point(px(0.), window_size.height - corner.height), corner).contains(&pos)
    {
        return Some(ResizeEdge::BottomLeft);
    }
    if !tiling.bottom
        && Bounds::new(
            point(
                window_size.width - corner.width,
                window_size.height - corner.height,
            ),
            corner,
        )
        .contains(&pos)
    {
        return Some(ResizeEdge::BottomRight);
    }

    if !tiling.top && pos.y < shadow_size {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - shadow_size {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < shadow_size {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - shadow_size {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_ui_theme::{MIN_EDGED_STEP, MIN_VISIBLE_STEP, ThemeRegistry, level_step};

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
}
