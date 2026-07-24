use crate::groups::{GROUP_ORDER, group_label};
use crate::theme::Palette;
use crate::{
    Drag, GUTTER_LABEL_SIZE, RULER_HEIGHT, Source, TRACKS_MENU_MAX_LIST, TRACKS_MENU_WIDTH,
    TraceView, scrollbar_canvas,
};
use bviz_trace_profile::{LaneClass, Profile};
use gpui::{
    AnyElement, App, Context, IntoElement, MouseButton, MouseDownEvent, Rgba, SharedString,
    deferred, div, prelude::*, px,
};

impl TraceView {
    /// The tracks visibility menu in the gutter corner where the ruler meets the
    /// track-label column: a trigger showing how many tracks are visible, and,
    /// when open, a filter field over a checklist of track names. A backdrop
    /// behind the open menu dismisses it on an outside click.
    pub(crate) fn tracks_menu(
        &self,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                this.tracks_menu_open = !this.tracks_menu_open;
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
            .w(px(self.gutter_width))
            .h(px(RULER_HEIGHT))
            .child(trigger)
            .children(self.tracks_menu_popover(palette, colors, cx))
    }

    /// The open tracks menu's deferred body: a filter field over the filtered
    /// track checklist, with a full-viewport backdrop behind it that closes the
    /// menu on an outside click. `None` when the menu is closed.
    fn tracks_menu_popover(
        &self,
        palette: Palette,
        colors: &bviz_ui_theme::Colors,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.tracks_menu_open {
            return None;
        }
        let profile = match self.model.read(cx).source() {
            Source::Loaded(p) => p.clone(),
            _ => return None,
        };
        let rows = self.menu_rows(&profile, palette, colors, cx);

        let menu = self.tracks_menu_chrome(rows, palette, colors, cx);
        let backdrop = self.tracks_menu_backdrop(cx);
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
        let filter = self.track_filter.read(cx).content().to_lowercase();
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
            let folded = self.menu_collapsed_groups.get(group).copied().unwrap_or(false);
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
                this.toggle_menu_group_collapsed(group);
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
                        this.clamp_lane_scroll(cx);
                        this.invalidate_chart(cx);
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
                this.clamp_lane_scroll(cx);
                this.invalidate_chart(cx);
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
    fn tracks_menu_chrome(
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
                    this.press_menu(e.position);
                    cx.stop_propagation();
                }),
            )
            .child(div().p_1().child(self.track_filter.clone()))
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
                        this.clamp_lane_scroll(cx);
                        this.invalidate_chart(cx);
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
            .track_scroll(&self.menu_scroll)
            .children(rows);
        let menu_scroll = self.menu_scroll.clone();
        let bar = scrollbar_canvas(
            self.menu_bar.clone(),
            cx.entity(),
            matches!(self.drag, Drag::MenuThumb(_)),
            palette.scroll_track,
            palette.scroll_thumb,
            move |bounds| {
                let visible = f32::from(bounds.size.height);
                let content = visible + f32::from(menu_scroll.max_offset().y);
                let offset = -f32::from(menu_scroll.offset().y);
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
    fn tracks_menu_backdrop(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let viewport = self.viewport.get();
        div()
            .absolute()
            .left_0()
            .top_0()
            .w(viewport.size.width)
            .h(viewport.size.height)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.tracks_menu_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }
}
