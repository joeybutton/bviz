//! A column of rows scrolling under a scroll bar drawn over its right edge.

use crate::drag::{DragEnd, track_drag};
use crate::layer::overlay_canvas;
use crate::scrollbar::{Axis, DEFAULT_MIN_THUMB, SCROLLBAR_THICKNESS, Scrollbar, ScrollbarColors};
use gpui::{
    App, Bounds, Context, Entity, IntoElement, Pixels, Point, ScrollHandle,
    UniformListScrollHandle, Window, point, px, size,
};

/// How far a column of rows has scrolled, and the drag on its bar. The column
/// tracks [`Self::handle`], or [`Self::uniform_handle`] where a uniform list
/// draws it, and keeps [`SCROLLBAR_THICKNESS`] clear at its right edge, where
/// [`list_scrollbar`] draws the bar.
pub struct ListScroll {
    handle: ScrollHandle,
    /// Scrolls `handle` for a uniform list, which reads its offset through it.
    uniform: UniformListScrollHandle,
    /// The grab offset from the thumb's leading edge while the bar is dragged.
    grab: Option<f32>,
}

impl Default for ListScroll {
    fn default() -> Self {
        let uniform = UniformListScrollHandle::new();
        let handle = uniform.0.borrow().base_handle.clone();
        Self {
            handle,
            uniform,
            grab: None,
        }
    }
}

impl ListScroll {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn handle(&self) -> &ScrollHandle {
        &self.handle
    }

    pub fn uniform_handle(&self) -> &UniformListScrollHandle {
        &self.uniform
    }

    /// The bar for the rows' content height against the height they are drawn
    /// in; `None` while they all fit.
    pub fn bar(&self) -> Option<Scrollbar> {
        let list = self.handle.bounds();
        let visible = f32::from(list.size.height);
        Scrollbar::new(
            Axis::Vertical,
            Bounds::new(
                point(list.right() - px(SCROLLBAR_THICKNESS), list.origin.y),
                size(px(SCROLLBAR_THICKNESS), list.size.height),
            ),
            visible + f32::from(self.handle.max_offset().y),
            visible,
            -f32::from(self.handle.offset().y),
            DEFAULT_MIN_THUMB,
        )
    }

    /// Begins a drag where `pos` lands on the bar, which a press on the track
    /// centres the thumb on, and reports whether it did; a press anywhere else
    /// is left to the row under it.
    pub fn press(&mut self, pos: Point<Pixels>) -> bool {
        let Some(bar) = self.bar().filter(|bar| bar.track_hit(pos)) else {
            return false;
        };
        let grab = bar.grab(pos);
        self.grab = Some(grab);
        self.thumb_to(pos, grab);
        true
    }

    /// Scrolls back to the first row.
    pub fn to_top(&self) {
        self.handle.set_offset(Point::default());
    }

    fn thumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        if let Some(bar) = self.bar() {
            let mut offset = self.handle.offset();
            offset.y = px(-bar.offset_at(pos, grab));
            self.handle.set_offset(offset);
        }
    }

    fn drag_to(&mut self, pos: Point<Pixels>) {
        if let Some(grab) = self.grab {
            self.thumb_to(pos, grab);
        }
    }
}

/// The bar of the rows whose [`ListScroll`] `scroll` reads and `scroll_mut`
/// moves in `view`, drawn over them once they are laid out, and carrying a
/// drag [`ListScroll::press`] began past the bar's edge.
pub fn list_scrollbar<V: 'static>(
    view: Entity<V>,
    scroll: fn(&V) -> &ListScroll,
    scroll_mut: fn(&mut V) -> &mut ListScroll,
    colors: ScrollbarColors,
) -> impl IntoElement {
    overlay_canvas(move |_, window: &mut Window, cx: &mut App| {
        let scroll = scroll(view.read(cx));
        let (bar, dragging) = (scroll.bar(), scroll.grab.is_some());
        if let Some(bar) = bar {
            bar.paint(colors.track, colors.thumb, window);
        }
        if dragging {
            track_drag(
                window,
                &view,
                move |view, pos, _: &mut Window, cx: &mut Context<V>| {
                    scroll_mut(view).drag_to(pos);
                    cx.notify();
                },
                move |view, _, _: DragEnd, _: &mut Window, cx: &mut Context<V>| {
                    scroll_mut(view).grab = None;
                    cx.notify();
                },
            );
        }
    })
}
