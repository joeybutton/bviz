//! A horizontal slider over a continuous range. Drag the thumb or click the
//! track to set the value; observe the entity to react to changes. An optional
//! marker draws a muted tick at a reference value (e.g. a default).

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, App, Bounds, Context, ElementId, IntoElement, MouseButton,
    MouseDownEvent, Pixels, Render, Window,
};

use crate::track_drag;

const WIDTH: f32 = 120.;
const HEIGHT: f32 = 18.;
const THUMB: f32 = 12.;
const RAIL_H: f32 = 4.;
const TICK_W: f32 = 2.;
const TICK_H: f32 = 12.;

pub struct Slider {
    id: ElementId,
    value: f32,
    min: f32,
    max: f32,
    /// Rounding applied to a dragged value, so a slider over integral spacing
    /// snaps rather than yielding sub-pixel noise.
    step: f32,
    /// A reference value drawn as a muted tick, or `None` for no marker.
    marker: Option<f32>,
    /// The track's window-space bounds, captured each paint so a press or drag
    /// can map a cursor x to a value.
    track: Rc<Cell<Bounds<Pixels>>>,
    dragging: bool,
}

impl Slider {
    pub fn new(
        id: impl Into<ElementId>,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
        marker: Option<f32>,
        _cx: &mut Context<Self>,
    ) -> Self {
        Self {
            id: id.into(),
            value: value.clamp(min, max),
            min,
            max,
            step,
            marker,
            track: Rc::new(Cell::new(Bounds::default())),
            dragging: false,
        }
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Set the value (clamped to the range), notifying observers if it changed.
    pub fn set_value(&mut self, v: f32, cx: &mut Context<Self>) {
        let v = v.clamp(self.min, self.max);
        if v != self.value {
            self.value = v;
            cx.notify();
        }
    }

    /// The snapped value a cursor at window-space `x` selects, using the track
    /// bounds captured during paint.
    fn value_from_x(&self, x: Pixels) -> f32 {
        let bounds = self.track.get();
        let usable = (f32::from(bounds.size.width) - THUMB).max(1.);
        let rel = f32::from(x) - f32::from(bounds.origin.x) - THUMB / 2.;
        let frac = (rel / usable).clamp(0., 1.);
        let raw = self.min + frac * (self.max - self.min);
        ((raw / self.step).round() * self.step).clamp(self.min, self.max)
    }

    fn frac(&self, v: f32) -> f32 {
        let span = (self.max - self.min).max(f32::EPSILON);
        ((v - self.min) / span).clamp(0., 1.)
    }
}

impl Render for Slider {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let usable = WIDTH - THUMB;
        let value_frac = self.frac(self.value);
        let thumb_left = value_frac * usable;

        let rail = div()
            .absolute()
            .left(px(THUMB / 2.))
            .top(px((HEIGHT - RAIL_H) / 2.))
            .w(px(usable))
            .h(px(RAIL_H))
            .rounded_full()
            .bg(colors.border);
        let fill = div()
            .absolute()
            .left(px(THUMB / 2.))
            .top(px((HEIGHT - RAIL_H) / 2.))
            .w(px(value_frac * usable))
            .h(px(RAIL_H))
            .rounded_full()
            .bg(colors.selected);
        let tick = self.marker.map(|m| {
            div()
                .absolute()
                .left(px(self.frac(m) * usable + THUMB / 2. - TICK_W / 2.))
                .top(px((HEIGHT - TICK_H) / 2.))
                .w(px(TICK_W))
                .h(px(TICK_H))
                .bg(colors.disabled)
        });
        let thumb = div()
            .absolute()
            .left(px(thumb_left))
            .top(px((HEIGHT - THUMB) / 2.))
            .size(px(THUMB))
            .rounded_full()
            .bg(colors.text)
            .border_1()
            .border_color(colors.background);

        // An invisible canvas over the track records its window-space bounds (so a
        // press maps to a value) and, while a drag is live, re-installs the
        // window-global move/up handlers each frame so the thumb keeps following
        // the cursor past the slider's own bounds.
        let track = self.track.clone();
        let entity = cx.entity();
        let dragging = self.dragging;
        let surface = canvas(
            move |bounds, _: &mut Window, _: &mut App| track.set(bounds),
            move |_, _, window: &mut Window, _: &mut App| {
                if dragging {
                    track_drag(
                        window,
                        &entity,
                        |this, e, cx| {
                            let v = this.value_from_x(e.position.x);
                            this.set_value(v, cx);
                        },
                        |this, _, cx| {
                            if this.dragging {
                                this.dragging = false;
                                cx.notify();
                            }
                        },
                    );
                }
            },
        )
        .absolute()
        .size_full();

        div()
            .id(self.id.clone())
            .relative()
            .w(px(WIDTH))
            .h(px(HEIGHT))
            .cursor_pointer()
            .child(rail)
            .child(fill)
            .children(tick)
            .child(thumb)
            .child(surface)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.dragging = true;
                    let v = this.value_from_x(e.position.x);
                    this.set_value(v, cx);
                    // Repaint so the canvas installs the drag handlers even when
                    // the press landed on the current value.
                    cx.notify();
                }),
            )
    }
}
