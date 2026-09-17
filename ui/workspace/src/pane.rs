use crate::{AnyPanel, PanelEvent};
use bviz_ui_components::Button;
use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyElement, ClickEvent, ElementId, EntityId, EventEmitter, IntoElement, MouseButton, Pixels,
    Render, Subscription, Window, div, prelude::*, px,
};
use std::cmp::Ordering;

/// Height of the tab strip, giving a tab's icon, label and close control a line
/// to sit on with room around them.
const TAB_STRIP_HEIGHT: Pixels = px(30.);

/// How wide a tab may grow before its label is cut, so one long title cannot
/// push its neighbours off the strip.
const MAX_TAB_WIDTH: Pixels = px(200.);

const TAB_ICON_SIZE: Pixels = px(14.);

/// A panel of a pane, and the subscription carrying that panel's events to the
/// pane. Taking the panel out of the pane retires the subscription with it.
struct Tab {
    panel: AnyPanel,
    _events: Subscription,
}

/// Panels sharing one place in a window, of which the pane shows one and lists
/// the rest in a tab strip above it. A pane holds at least one panel, `active`
/// indexes the one it shows, and closing the last panel closes the pane itself
/// ([`PaneEvent::Closed`]) rather than leaving an empty one in the window.
pub struct Pane {
    tabs: Vec<Tab>,
    active: usize,
}

/// What a [`Pane`] tells its workspace about itself.
pub enum PaneEvent {
    /// The panel on show changed, or changed what it says about itself.
    Changed,
    /// The last panel closed, so the pane is done.
    Closed,
    /// The panel on show is asking for a window of its own.
    Detach,
}

impl EventEmitter<PaneEvent> for Pane {}

impl Pane {
    pub(crate) fn new(panel: AnyPanel, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let events = panel.subscribe(window, cx);
        panel.set_active(true, window, cx);
        Self {
            tabs: vec![Tab {
                panel,
                _events: events,
            }],
            active: 0,
        }
    }

    /// Adds `panel` to the pane and shows it.
    pub(crate) fn add(&mut self, panel: AnyPanel, window: &mut Window, cx: &mut Context<Self>) {
        let events = panel.subscribe(window, cx);
        self.tabs.push(Tab {
            panel,
            _events: events,
        });
        self.activate(self.tabs.len() - 1, window, cx);
    }

    pub fn active_panel(&self) -> &AnyPanel {
        &self.tabs[self.active].panel
    }

    /// Which tab holds `panel`, if this pane holds it at all.
    pub(crate) fn index_of(&self, panel: EntityId) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.panel.entity_id() == panel)
    }

    /// The tab the pane shows.
    pub(crate) fn showing(&self) -> usize {
        self.active
    }

    /// Shows the panel at `index` in place of the one on show.
    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index == self.active || index >= self.tabs.len() {
            return;
        }
        self.tabs[self.active].panel.set_active(false, window, cx);
        self.active = index;
        self.show_active(window, cx);
    }

    /// Shows the panel after the one on show, wrapping round to the first.
    pub fn activate_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.activate((self.active + 1) % self.tabs.len(), window, cx);
    }

    /// Shows the panel before the one on show, wrapping round to the last.
    pub fn activate_previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.tabs.len() - 1;
        self.activate(self.active.checked_sub(1).unwrap_or(last), window, cx);
    }

    /// Closes the panel at `index`, or the whole pane where that panel is the
    /// only one it holds.
    pub fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 {
            cx.emit(PaneEvent::Closed);
            return;
        }
        drop(self.detach(index, window, cx));
    }

    pub fn close_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close(self.active, window, cx);
    }

    /// Hands the panel on show to a caller that gives it a place of its own,
    /// leaving the pane showing another. `None` where it is the pane's only
    /// panel, which cannot leave without emptying the pane.
    pub(crate) fn take_active(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyPanel> {
        self.detach(self.active, window, cx)
    }

    pub(crate) fn remove_active(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyPanel {
        self.give_up(self.active, window, cx)
    }

    /// Hands `panel` over where this pane holds it, `None` where it does not.
    pub(crate) fn remove(
        &mut self,
        panel: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyPanel> {
        let index = self.index_of(panel)?;
        Some(self.give_up(index, window, cx))
    }

    /// Hands the panel at `index` to a caller that re-homes it in another
    /// window, whether or not the pane holds others. Giving up its only panel
    /// leaves the pane with nothing to show, so it is done
    /// ([`PaneEvent::Closed`]) just as closing that panel leaves it.
    fn give_up(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyPanel {
        if let Some(panel) = self.detach(index, window, cx) {
            return panel;
        }
        let panel = self.active_panel().clone();
        panel.set_active(false, window, cx);
        cx.emit(PaneEvent::Closed);
        panel
    }

    pub(crate) fn on_panel_event(
        &mut self,
        panel: EntityId,
        event: &PanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.index_of(panel) else {
            return;
        };
        match event {
            PanelEvent::TitleChanged | PanelEvent::StatusChanged => {
                if index == self.active {
                    cx.emit(PaneEvent::Changed);
                }
                cx.notify();
            }
            PanelEvent::Close => self.close(index, window, cx),
        }
    }

    /// Takes the panel at `index` out of the pane, moving on to another panel
    /// where it was the one on show. `None` where the pane does not hold such a
    /// panel, or holds only it.
    fn detach(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyPanel> {
        if index >= self.tabs.len() || self.tabs.len() == 1 {
            return None;
        }
        let taken = self.tabs.remove(index).panel;
        let showing = self.active;
        self.active = showing_after(showing, index, self.tabs.len());
        if index == showing {
            taken.set_active(false, window, cx);
            self.show_active(window, cx);
        } else {
            cx.notify();
        }
        Some(taken)
    }

    /// Brings the panel `active` indexes forward, and tells the workspace what
    /// the pane is showing.
    fn show_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.active_panel().clone();
        panel.set_active(true, window, cx);
        // A repaint the panel marked while its pane showed another was consumed
        // by a frame that left it out, so the frame bringing it forward records
        // the scene it draws only if it is marked again here.
        panel.notify(cx);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    fn tab_strip(&self, colors: &ThemeColors, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = Vec::with_capacity(self.tabs.len());
        for index in 0..self.tabs.len() {
            tabs.push(self.tab(index, colors, cx));
        }
        div()
            .flex()
            .flex_row()
            .items_center()
            .flex_shrink_0()
            .w_full()
            .h(TAB_STRIP_HEIGHT)
            .overflow_hidden()
            .bg(colors.surface)
            .border_b_1()
            .border_color(colors.border)
            .text_sm()
            .children(tabs)
    }

    fn tab(&self, index: usize, colors: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.tabs[index].panel.clone();
        let showing = index == self.active;
        let ink = if showing {
            colors.text
        } else {
            colors.text_muted
        };
        let hover = colors.element_hover;
        let pane = cx.entity();
        div()
            .id(ElementId::NamedInteger(
                "pane-tab".into(),
                panel.entity_id().as_u64(),
            ))
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .h_full()
            .px_2()
            .max_w(MAX_TAB_WIDTH)
            .border_r_1()
            .border_color(colors.border)
            .bg(if showing {
                colors.background
            } else {
                colors.surface
            })
            .text_color(ink)
            .cursor_pointer()
            .when(!showing, |tab| tab.hover(move |s| s.bg(hover)))
            // A second click on a tab sends its panel to a window of its own. The
            // workspace detaches whichever panel the pane is showing, so the tab
            // is brought forward either way.
            .on_click(cx.listener(move |pane, event: &ClickEvent, window, cx| {
                pane.activate(index, window, cx);
                if event.click_count() == 2 {
                    cx.emit(PaneEvent::Detach);
                }
            }))
            .children(panel.icon(cx).map(|name| {
                bviz_ui_assets::icon(name)
                    .size(TAB_ICON_SIZE)
                    .text_color(ink)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .min_w(px(0.))
                    .child(div().min_w(px(0.)).truncate().child(panel.plain_title(cx)))
                    // A cut-off title must still show which document the tab
                    // belongs to.
                    .children(
                        panel
                            .title_suffix()
                            .map(|suffix| div().flex_shrink_0().whitespace_nowrap().child(suffix)),
                    ),
            )
            .child(
                div()
                    // A press on the close control must not also reach the tab
                    // and bring forward the panel it is about to close.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new(
                            ElementId::NamedInteger(
                                "pane-tab-close".into(),
                                panel.entity_id().as_u64(),
                            ),
                            move |_, window, cx| {
                                pane.update(cx, |pane, cx| pane.close(index, window, cx));
                            },
                        )
                        .ghost()
                        .icon("x")
                        .compact(),
                    ),
            )
            .into_any_element()
    }
}

/// Which of the `remaining` tabs a pane shows once the tab at `removed` has left
/// it and the tab at `showing` was the one on show: the tab that moves up into
/// the gap, or the last tab where the gap is past the end; and otherwise the
/// same tab, one place nearer the front where the tab that left sat before it.
fn showing_after(showing: usize, removed: usize, remaining: usize) -> usize {
    match removed.cmp(&showing) {
        Ordering::Equal => removed.min(remaining - 1),
        Ordering::Less => showing - 1,
        Ordering::Greater => showing,
    }
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = self.active_panel().view();
        // One panel is the pane: there is nothing to pick between, and a strip
        // of one tab would only take room from the panel.
        if self.tabs.len() == 1 {
            return body.into_any_element();
        }
        let colors = bviz_ui_theme::theme(window, cx).colors;
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.tab_strip(&colors, cx))
            .child(div().flex().flex_col().flex_1().min_h(px(0.)).child(body))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel handed over is the one on show, so the tab left showing is the
    /// one that moves up into the gap it leaves.
    #[test]
    fn the_tab_after_the_one_given_up_takes_its_place() {
        assert_eq!(showing_after(0, 0, 2), 0);
        assert_eq!(showing_after(1, 1, 3), 1);
    }

    /// There is no tab after the last one, so the strip falls back to the tab
    /// that becomes the last.
    #[test]
    fn giving_up_the_last_tab_falls_back_to_the_one_before_it() {
        assert_eq!(showing_after(2, 2, 2), 1);
        assert_eq!(showing_after(1, 1, 1), 0);
    }

    /// A tab taken from after the one on show leaves it where it is.
    #[test]
    fn a_tab_after_the_one_on_show_leaves_it_alone() {
        assert_eq!(showing_after(1, 2, 3), 1);
    }

    /// A tab taken from before the one on show moves it one place nearer the
    /// front, so the pane goes on showing the same panel.
    #[test]
    fn a_tab_before_the_one_on_show_moves_it_nearer_the_front() {
        assert_eq!(showing_after(2, 0, 3), 1);
        assert_eq!(showing_after(1, 0, 2), 0);
    }
}
