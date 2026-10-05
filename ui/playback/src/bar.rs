use crate::{Clock, Speed, TEXT_SIZE, Timeline};
use bviz_invocation::EpochMicros;
use bviz_ui_components::{
    Button, Dropdown, DropdownEvent, ScrubberColors, ScrubberState, TextTooltip, TooltipColors,
    scrubber,
};
use bviz_ui_theme::ThemeColors;
use gpui::{
    AnyElement, App, Context, Div, Entity, IntoElement, Pixels, Render, SharedString, Subscription,
    Window, div, prelude::*, px, relative,
};
use std::time::Duration;

/// Height of the band the bar draws.
const BAR_HEIGHT: Pixels = px(40.);

/// How wide the document's name may grow before it is cut, so a long one
/// leaves the scrubber room.
const NAME_MAX_WIDTH: Pixels = px(240.);

/// Width held for the time readout, wide enough for the longest it reads, so
/// the scrubber does not shift as the time changes.
const READOUT_WIDTH: Pixels = px(128.);

const SECONDS_PER_MINUTE: u64 = 60;

/// Size of the name a band of the scrubber is labelled with, small enough to
/// sit above the knob.
const BAND_TEXT_SIZE: Pixels = px(10.);

/// How far every other band of the scrubber is carried from the bar's fill
/// toward its text, so neighbouring bands are told apart.
const BAND_SHADE: f32 = 0.06;

/// The transport driving a document's [`Clock`], labelled with the document's
/// name. Draws nothing while the clock has no time.
pub struct PlaybackBar {
    name: SharedString,
    clock: Entity<Clock>,
    /// Shows the clock's speed, and sets it when the user picks another.
    speed: Entity<Dropdown>,
    scrubber: ScrubberState,
    _clock_changed: Subscription,
    _speed_chosen: Subscription,
}

impl PlaybackBar {
    pub fn new(name: SharedString, clock: Entity<Clock>, cx: &mut Context<Self>) -> Self {
        let current = speed_of(&clock, cx);
        let labels = Speed::all().map(|speed| format!("{}\u{d7}", speed.factor()));
        let speed = cx.new(|cx| Dropdown::new("playback-speed", labels, current.index(), cx));
        let clock_changed = cx.observe(&clock, |bar, clock, cx| {
            let index = speed_of(&clock, cx).index();
            bar.speed
                .update(cx, |dropdown, cx| dropdown.set_selected(index, cx));
            cx.notify();
        });
        let speed_chosen = cx.subscribe(&speed, |bar, _, event: &DropdownEvent, cx| {
            let DropdownEvent::Changed(index) = event;
            if let Some(speed) = Speed::from_index(*index) {
                bar.clock.update(cx, |clock, cx| clock.set_speed(speed, cx));
            }
        });
        Self {
            name,
            clock,
            speed,
            scrubber: ScrubberState::default(),
            _clock_changed: clock_changed,
            _speed_chosen: speed_chosen,
        }
    }

    pub fn clock(&self) -> &Entity<Clock> {
        &self.clock
    }

    /// Seeks to `fraction` of the way through the range. Grabbing the scrubber
    /// pauses; the drag that follows only seeks, so a play toggled from the
    /// keyboard mid-drag stands.
    fn scrub_to(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let held = self.scrubber.held();
        self.clock.update(cx, |clock, cx| {
            let Some(range) = clock.timeline().map(Timeline::range) else {
                return;
            };
            if !held {
                clock.pause(cx);
            }
            clock.seek(range.at_fraction(fraction), cx);
        });
    }

    fn transport(&self, playing: bool) -> [Button; 3] {
        let drive =
            |id: &'static str, icon: &'static str, gesture: fn(&mut Clock, &mut Context<Clock>)| {
                let clock = self.clock.clone();
                Button::new(id, move |_, _, cx| clock.update(cx, gesture))
                    .ghost()
                    .icon(icon)
            };
        [
            drive("playback-step-back", "skip-back", |clock, cx| {
                clock.step(false, cx);
            }),
            drive(
                "playback-play-pause",
                if playing { "pause" } else { "play" },
                Clock::play_pause,
            ),
            drive("playback-step-forward", "skip-forward", |clock, cx| {
                clock.step(true, cx);
            }),
        ]
    }

    /// The scrubber, laid over `bands`.
    fn scrubber(
        &self,
        bands: Div,
        fraction: f32,
        colors: &ThemeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .relative()
            .flex()
            .flex_1()
            .h_full()
            .child(bands)
            .child(self.track(fraction, colors, cx))
            .into_any_element()
    }

    fn track(&self, fraction: f32, colors: &ThemeColors, cx: &mut Context<Self>) -> AnyElement {
        // The played portion and the knob mark a position within the span the
        // track carries, so they take the text they are read among.
        let fills = ScrubberColors {
            track: colors.element_hover.into(),
            played: colors.text.into(),
            knob: colors.text.into(),
        };
        scrubber(
            "playback-scrubber",
            fraction,
            fills,
            |bar: &mut Self| &mut bar.scrubber,
            cx.entity(),
            |bar, fraction, cx| bar.scrub_to(fraction, cx),
        )
        .into_any_element()
    }
}

impl Render for PlaybackBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let clock = self.clock.read(cx);
        let Some(timeline) = clock.timeline() else {
            return div().into_any_element();
        };
        Clock::keep_pace(Some(&self.clock), window, cx);
        let now = timeline.now();
        let fraction = timeline.range().fraction_at(now);
        let readout = readout(timeline, now);
        let playing = timeline.is_playing();
        let colors = bviz_ui_theme::theme(window, cx).colors;
        let bands = bands(timeline, &colors);
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .h(BAR_HEIGHT)
            .w_full()
            .bg(colors.surface)
            .border_t_1()
            .border_color(colors.border)
            .font_family(bviz_ui_assets::UI)
            .text_color(colors.text)
            .child(
                div()
                    .max_w(NAME_MAX_WIDTH)
                    .flex_none()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(TEXT_SIZE)
                    .text_color(colors.text_muted)
                    .child(self.name.clone()),
            )
            .children(self.transport(playing))
            .child(self.scrubber(bands, fraction, &colors, cx))
            .child(
                div()
                    .w(READOUT_WIDTH)
                    .flex_none()
                    .text_size(TEXT_SIZE)
                    .child(readout),
            )
            .child(self.speed.clone())
            .into_any_element()
    }
}

/// Each band of `timeline` as the stretch of the scrubber it spans, labelled
/// with its name, which hovering it shows whole.
fn bands(timeline: &Timeline, colors: &ThemeColors) -> Div {
    let range = timeline.range();
    let tooltip = TooltipColors::from_colors(colors);
    let shade = bviz_ui_theme::mix(colors.surface, colors.text, BAND_SHADE);
    let spanning = timeline.bands().iter().filter_map(|band| {
        let from = range.fraction_at(band.span().start());
        let to = range.fraction_at(band.span().end());
        (to > from).then_some((band, from, to))
    });
    let spanned = spanning.enumerate().map(|(i, (band, from, to))| {
        div()
            .id(("playback-band", i))
            .absolute()
            .top_0()
            .h_full()
            .left(relative(from))
            .w(relative(to - from))
            .when(i % 2 == 1, |band| band.bg(shade))
            .border_l_1()
            .border_color(colors.border)
            .pl_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_size(BAND_TEXT_SIZE)
            .text_color(colors.text_muted)
            .child(band.name().clone())
            .tooltip(TextTooltip::builder(band.name().clone(), tooltip))
    });
    div().absolute().inset_0().children(spanned)
}

fn speed_of(clock: &Entity<Clock>, cx: &App) -> Speed {
    clock
        .read(cx)
        .timeline()
        .map_or(Speed::default(), Timeline::speed)
}

/// `now` and the range's length, each as time since the range's start; the
/// first marked approximate where the timeline's times are.
fn readout(timeline: &Timeline, now: EpochMicros) -> SharedString {
    let range = timeline.range();
    let elapsed = minutes_seconds(range.elapsed_micros(now));
    let total = minutes_seconds(range.length_micros());
    let approximate = if timeline.approximate() { "~" } else { "" };
    format!("{approximate}{elapsed} / {total}").into()
}

/// `micros` as `M:SS`, to the nearest second.
fn minutes_seconds(micros: i64) -> String {
    let seconds = Duration::from_micros(micros.max(0).unsigned_abs())
        .as_secs_f64()
        .round() as u64;
    format!(
        "{}:{:02}",
        seconds / SECONDS_PER_MINUTE,
        seconds % SECONDS_PER_MINUTE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_reads_as_minutes_and_seconds() {
        assert_eq!(minutes_seconds(0), "0:00");
        assert_eq!(minutes_seconds(59_600_000), "1:00");
        assert_eq!(minutes_seconds(754_000_000), "12:34");
        assert_eq!(minutes_seconds(-5), "0:00");
    }
}
