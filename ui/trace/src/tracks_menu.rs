use crate::groups::{GROUP_ORDER, default_collapsed_groups, group_label};
use crate::model::TraceModel;
use crate::theme::Palette;
use crate::{
    GUTTER_LABEL_SIZE, RULER_HEIGHT, Source, TRACKS_MENU_MAX_LIST, TRACKS_MENU_WIDTH, TraceView,
    set_list_thumb, thumb_offset,
};
use bviz_trace_profile::{LaneClass, Profile};
use bviz_ui_components::{Axis, DEFAULT_MIN_THUMB, Scrollbar, TextInput, track_drag};
use gpui::{
    AnyElement, App, Bounds, Context, Entity, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Rgba, ScrollHandle, SharedString,
    Subscription, WeakEntity, Window, canvas, deferred, div, point, prelude::*, px, size,
};

/// What a left-button drag over the tracks menu is currently moving.
#[derive(Clone, Copy)]
enum MenuDrag {
    None,
    /// A press on the scroll thumb/track, holding that grab offset from the
    /// thumb's leading edge.
    Thumb(f32),
}

/// The tracks-visibility menu as a self-contained popover: a trigger in the
/// gutter corner where the ruler meets the track-label column, and, when open, a
/// filter field over a checklist of track names grouped by lane class. Owns its
/// open, fold, filter, scroll, and drag state as plain fields. Reads the visible
/// tracks from [`TraceModel`] and writes visibility
/// there; reflows the timeline through the owning [`TraceView`].
pub(crate) struct TracksMenu {
    /// Whether the menu is open.
    open: bool,
    /// Which lane-class groups are folded in the menu, indexed by [`GROUP_ORDER`];
    /// independent of the chart's collapse so the menu can manage a group's
    /// visibility while its lanes are collapsed in the chart.
    collapsed_groups: [bool; GROUP_ORDER.len()],
    /// The filter field; its text narrows the listed tracks.
    filter: Entity<TextInput>,
    /// Re-renders the menu as the filter text changes; `TextInput` notifies on
    /// every edit.
    _filter_observer: Subscription,
    /// Scroll position of the track list.
    scroll: ScrollHandle,
    /// The scroll-bar geometry set during paint, read by mouse handlers; `None`
    /// when the list fits and so shows no bar.
    bar: Option<Scrollbar>,
    drag: MenuDrag,
    /// Read for the visible tracks, written to toggle them.
    model: Entity<TraceModel>,
    /// The owning view, read for the gutter width and viewport that position the
    /// popover, and updated to reflow the timeline after a visibility change.
    shell: WeakEntity<TraceView>,
}

impl TracksMenu {
    pub(crate) fn new(
        model: Entity<TraceModel>,
        shell: WeakEntity<TraceView>,
        cx: &mut Context<Self>,
    ) -> Self {
        let filter = cx.new(|cx| TextInput::new(cx, "Filter tracks…", 0));
        let filter_observer = cx.observe(&filter, |_, _, cx| cx.notify());
        Self {
            open: false,
            collapsed_groups: default_collapsed_groups(),
            filter,
            _filter_observer: filter_observer,
            scroll: ScrollHandle::new(),
            bar: None,
            drag: MenuDrag::None,
            model,
            shell,
        }
    }

    /// Resets the view-local fold state and list scroll for a freshly loaded
    /// profile. The document-derived visibility is seeded by the model instead.
    pub(crate) fn reset_for_new_profile(&mut self) {
        self.collapsed_groups = default_collapsed_groups();
        set_list_thumb(&self.scroll, 0.0);
    }

    /// Reflows the timeline after a visibility change: clamps its lane scroll to
    /// the new layout height and rebuilds its scene.
    fn reflow_timeline(&self, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |tv, cx| {
                tv.clamp_timeline_scroll(cx);
                tv.invalidate_timeline(cx);
            });
        }
    }

    /// Folds or unfolds a lane-class group's track list.
    fn toggle_group_collapsed(&mut self, group: usize) {
        if let Some(c) = self.collapsed_groups.get_mut(group) {
            *c = !*c;
        }
    }

    /// Begins a scroll drag if `pos` hit the bar; otherwise leaves the press to
    /// fall through.
    fn press(&mut self, pos: Point<Pixels>) {
        if let Some(bar) = self.bar {
            if bar.track_hit(pos) {
                let grab = bar.grab(pos);
                self.drag = MenuDrag::Thumb(grab);
                self.thumb_to(pos, grab);
            }
        }
    }

    /// Maps a thumb drag back to the list's scroll offset.
    fn thumb_to(&mut self, pos: Point<Pixels>, grab: f32) {
        if let Some(offset) = thumb_offset(self.bar, pos, grab) {
            set_list_thumb(&self.scroll, offset);
        }
    }

    /// The open menu's deferred body: a filter field over the filtered track
    /// checklist, with a full-viewport backdrop behind it that closes the menu on
    /// an outside click. `None` when the menu is closed.
    fn popover(
        &self,
        viewport: Bounds<Pixels>,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.open {
            return None;
        }
        let profile = match self.model.read(cx).source() {
            Source::Loaded(p) => p.clone(),
            _ => return None,
        };
        let rows = self.menu_rows(&profile, palette, colors, cx);
        let menu = self.chrome(rows, palette, colors, cx);
        let backdrop = self.backdrop(viewport, cx);
        Some(deferred(div().child(backdrop).child(menu)))
    }

    /// Tracks under their lane-class group headers, each group's tracks sorted
    /// busiest first — the same grouping and order the chart lays out.
    fn menu_rows(
        &self,
        profile: &Profile,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let selected = colors.selected;
        let hover_bg = bviz_ui_theme::mix(colors.container, colors.background, 0.2);
        let filter = self.filter.read(cx).content().to_lowercase();
        let threads = profile.threads();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (group, &class) in GROUP_ORDER.iter().enumerate() {
            let mut tracks: Vec<usize> = (0..threads.len())
                .filter(|&t| threads[t].lane_class == class)
                .filter(|&t| {
                    filter.is_empty()
                        || profile.str(threads[t].name).to_lowercase().contains(&filter)
                })
                .collect();
            if tracks.is_empty() {
                continue;
            }
            crate::groups::sort_tracks_by_activity(profile, threads, &mut tracks);
            let folded = self.collapsed_groups.get(group).copied().unwrap_or(false);
            rows.push(
                self.group_row(group, class, &tracks, folded, palette, hover_bg, selected, cx)
                    .into_any_element(),
            );
            if folded {
                continue;
            }
            for i in tracks {
                let name = profile.str(threads[i].name);
                let visible = self.model.read(cx).visible_tracks().get(i).copied().unwrap_or(true);
                rows.push(
                    self.track_row(i, name, visible, palette, selected, cx).into_any_element(),
                );
            }
        }
        rows
    }

    /// A group header: a chevron and the group's visible/total count, folding the
    /// group's track list on click. Its trailing control toggles the whole
    /// group's visibility without also folding.
    fn group_row(
        &self,
        group: usize,
        class: LaneClass,
        tracks: &[usize],
        folded: bool,
        palette: Palette,
        hover_bg: Rgba,
        selected: Rgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let n_visible = {
            let model = self.model.read(cx);
            tracks
                .iter()
                .filter(|&&t| model.visible_tracks().get(t).copied().unwrap_or(true))
                .count()
        };
        let group_all_visible = n_visible == tracks.len();
        let chevron = if folded { "▸" } else { "▾" };
        div()
            .id(SharedString::from(format!("track-group-{group}")))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .mt_1()
            .px_2()
            .py_1()
            .cursor_pointer()
            .text_color(palette.label)
            .hover(move |s| s.bg(hover_bg))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_group_collapsed(group);
                cx.notify();
            }))
            .child(format!(
                "{chevron} {}  {}/{}",
                group_label(class),
                n_visible,
                tracks.len()
            ))
            .child(
                div()
                    .id(SharedString::from(format!("track-group-vis-{group}")))
                    .px_1()
                    .rounded_sm()
                    .hover(move |s| s.bg(selected))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.model.update(cx, |m, _| m.set_group_visible(group, !group_all_visible));
                        this.reflow_timeline(cx);
                        cx.notify();
                    }))
                    .child(if group_all_visible { "hide" } else { "show" }),
            )
    }

    /// A track row: a visibility checkbox and the track name, toggling the track
    /// on click.
    fn track_row(
        &self,
        i: usize,
        name: &str,
        visible: bool,
        palette: Palette,
        selected: Rgba,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let check = div()
            .size(px(13.))
            .flex_none()
            .rounded_sm()
            .border_1()
            .border_color(palette.label)
            .when(visible, |c| c.bg(selected).border_color(selected));
        div()
            .id(SharedString::from(format!("track-row-{i}")))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .pl_4()
            .pr_2()
            .py_1()
            .cursor_pointer()
            .rounded_sm()
            .hover(move |s| s.bg(selected))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model.update(cx, |m, _| m.toggle_track(i));
                this.reflow_timeline(cx);
                cx.notify();
            }))
            .child(check)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name.to_string()),
            )
    }

    /// The menu container over the built rows: the filter field, a show/hide-all
    /// toggle, and the scrollable track list with its scroll bar. Clicks inside
    /// must not reach the backdrop and close it; a press on the scroll bar grabs
    /// its thumb.
    fn chrome(
        &self,
        rows: Vec<AnyElement>,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (container, text) = (colors.container, colors.text);
        let hover_bg = bviz_ui_theme::mix(colors.container, colors.background, 0.2);
        div()
            .occlude()
            .absolute()
            .left_0()
            .top_0()
            .w(px(TRACKS_MENU_WIDTH))
            .flex()
            .flex_col()
            .bg(container)
            .border_1()
            .border_color(palette.border)
            .rounded_md()
            .shadow_lg()
            .text_size(px(GUTTER_LABEL_SIZE))
            .text_color(text)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.press(e.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .child(div().p_1().child(self.filter.clone()))
            .child({
                let (nvis, ntot) = self.model.read(cx).visible_count();
                let all_visible = ntot > 0 && nvis == ntot;
                div()
                    .id("tracks-toggle-all")
                    .flex()
                    .flex_row()
                    .items_center()
                    .mx_1()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(palette.border)
                    .hover(move |s| s.bg(hover_bg))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.update(cx, |m, _| m.set_all_visible(!all_visible));
                        this.reflow_timeline(cx);
                        cx.notify();
                    }))
                    .child(if all_visible { "Hide all" } else { "Show all" })
            })
            .child(self.menu_list(rows, palette, cx))
    }

    /// The scrollable track list with a scroll bar over its right edge; the bar
    /// reads the list's scroll handle to place the thumb and stashes the geometry
    /// for the menu's press hit-test.
    fn menu_list(
        &self,
        rows: Vec<AnyElement>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let list = div()
            .id("tracks-menu-list")
            .flex()
            .flex_col()
            .w_full()
            .p_1()
            .max_h(px(TRACKS_MENU_MAX_LIST))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);
        let scroll = self.scroll.clone();
        let bar = menu_scrollbar(
            cx.entity(),
            matches!(self.drag, MenuDrag::Thumb(_)),
            palette.scroll_track,
            palette.scroll_thumb,
            move |bounds| {
                let visible = f32::from(bounds.size.height);
                let content = visible + f32::from(scroll.max_offset().y);
                let offset = -f32::from(scroll.offset().y);
                (content, visible, offset)
            },
        );
        div()
            .relative()
            .w_full()
            .flex()
            .min_h_0()
            .child(list)
            .child(bar)
    }

    /// A backdrop the size of the viewport, behind the menu, closing it on an
    /// outside click.
    fn backdrop(&self, viewport: Bounds<Pixels>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div()
            .absolute()
            .left_0()
            .top_0()
            .w(viewport.size.width)
            .h(viewport.size.height)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }
}

impl Render for TracksMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = bviz_ui_theme::colors(window);
        let palette = Palette::from_colors(&colors);
        let Some(shell) = self.shell.upgrade() else {
            return div().into_any_element();
        };
        let (gutter_width, viewport) = {
            let s = shell.read(cx);
            (s.gutter_width, s.timeline.read(cx).viewport())
        };
        let (nvis, ntot) = self.model.read(cx).visible_count();
        let hover_bg = bviz_ui_theme::mix(colors.container, colors.background, 0.2);
        let trigger = div()
            .id("tracks-menu")
            .size_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .gap_1()
            .cursor_pointer()
            .text_size(px(GUTTER_LABEL_SIZE))
            .text_color(palette.label)
            .bg(palette.header_band)
            .hover(move |s| s.bg(hover_bg))
            // Keep the press off the chart handlers underneath.
            .on_mouse_down(MouseButton::Left, |_, _, cx: &mut App| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| {
                this.open = !this.open;
                cx.notify();
            }))
            .child(div().overflow_hidden().whitespace_nowrap().child(format!("{nvis}/{ntot} tracks")))
            .child(
                bviz_ui_assets::icon("caret-down")
                    .size(px(12.))
                    .text_color(palette.label),
            );
        // An anchor at the corner holds the trigger and, below it, the popover.
        // The popover is an absolute child of this anchor (not a flex sibling of
        // the trigger row), so it opens directly under the trigger and does not
        // shift the trigger's own layout.
        div()
            .absolute()
            .left_0()
            .top_0()
            .w(px(gutter_width))
            .h(px(RULER_HEIGHT))
            .child(trigger)
            .children(self.popover(viewport, palette, &colors, cx))
            .into_any_element()
    }
}

/// The tracks menu's vertical scroll bar, painted over the right edge of its
/// track list. Each frame the canvas reads `extents` (content extent, visible
/// extent, scroll offset) to place the thumb, paints it, stashes the geometry in
/// the menu's `bar` for hit-testing, and re-installs the drag handlers while
/// `dragging`.
fn menu_scrollbar(
    entity: Entity<TracksMenu>,
    dragging: bool,
    track: Rgba,
    thumb: Rgba,
    extents: impl Fn(Bounds<Pixels>) -> (f32, f32, f32) + 'static,
) -> impl IntoElement {
    canvas(
        move |_, _, _| {},
        move |bounds, _, window: &mut Window, cx: &mut App| {
            let (content, visible, offset) = extents(bounds);
            let layout = Scrollbar::new(
                Axis::Vertical,
                Bounds::new(
                    point(bounds.origin.x + bounds.size.width - px(crate::BAR_WIDTH), bounds.origin.y),
                    size(px(crate::BAR_WIDTH), bounds.size.height),
                ),
                content,
                visible,
                offset,
                DEFAULT_MIN_THUMB,
            );
            if let Some(painted) = layout {
                painted.paint(track, thumb, window);
            }
            entity.update(cx, |menu, _| menu.bar = layout);
            if dragging {
                track_drag(window, &entity, menu_on_drag, menu_end_drag);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// Routes an in-progress thumb drag to the list's scroll offset.
fn menu_on_drag(menu: &mut TracksMenu, event: &MouseMoveEvent, cx: &mut Context<TracksMenu>) {
    if let MenuDrag::Thumb(grab) = menu.drag {
        menu.thumb_to(event.position, grab);
    }
    cx.notify();
}

fn menu_end_drag(menu: &mut TracksMenu, _: &MouseUpEvent, cx: &mut Context<TracksMenu>) {
    menu.drag = MenuDrag::None;
    cx.notify();
}
