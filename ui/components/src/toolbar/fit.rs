//! Where each item of a one-row toolbar stands at a given width.

use gpui::Pixels;

/// How much a group holds out against the row narrowing: the higher, the
/// longer it keeps its wider presentations and stays in the row.
pub type Priority = u8;

/// What the fit reads of one item of the row.
pub(crate) struct Measure {
    pub kind: Kind,
    pub side: Side,
}

pub(crate) enum Kind {
    Group {
        /// The width of each of the group's presentations, in the order it
        /// gives them up, its widest first.
        widths: Vec<Pixels>,
        priority: Priority,
        /// Whether a rule parts the group from a control standing before it.
        parted: bool,
    },
    /// An item taking the room the groups leave, at `least` until every
    /// group that can has moved into the overflow menu, then at `floor`
    /// (see [`narrowings`]); narrower only past the last step.
    Filling { least: Pixels, floor: Pixels },
}

/// Which edge of the row an item is held against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Side {
    Leading,
    Trailing,
}

/// What the row is laid out in.
#[derive(Clone, Copy)]
pub(crate) struct Room {
    pub width: Pixels,
    /// Between neighbouring items of the row.
    pub gap: Pixels,
    /// The trigger of the overflow menu, an item of the row ending the leading
    /// ones while any group stands in the menu.
    pub overflow: Pixels,
    /// A rule parting a group from the control before it, with the gap after
    /// the rule.
    pub rule: Pixels,
}

/// Where one item stands.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Spot {
    /// In the row, in its presentation at index `stage`, spanning `width` from
    /// `x` past the row's left edge, its rule first where `ruled`.
    Row {
        stage: usize,
        x: Pixels,
        width: Pixels,
        ruled: bool,
    },
    Menu,
    /// Out of the row and the menu: a group in the menu whose trigger the row
    /// has no room for.
    Out,
}

/// Where a row's items stand, and the room between its two ends.
#[derive(PartialEq, Debug)]
pub(crate) struct Fitted {
    pub spots: Vec<Spot>,
    /// Where the overflow menu's trigger stands, if it does.
    pub trigger: Option<Pixels>,
    /// The room no item takes between the leading items and the trailing
    /// ones, as the offsets of its edges past the row's left edge.
    pub free: (Pixels, Pixels),
}

impl Fitted {
    /// Where along the row the item at `at` is reached, as the offset of its
    /// left edge past the row's and its width: its own span, short of its
    /// rule, while it stands in the row, and the overflow menu trigger's while
    /// it stands in the menu.
    pub fn reached_at(&self, at: usize, room: Room) -> Option<(Pixels, Pixels)> {
        match self.spots[at] {
            Spot::Row {
                x, width, ruled, ..
            } => Some(match ruled {
                true => (x + room.rule, width - room.rule),
                false => (x, width),
            }),
            Spot::Menu => self.trigger.map(|x| (x, room.overflow)),
            Spot::Out => None,
        }
    }
}

/// How `items` stand in `room`: the first state of [`narrowings`] whose row
/// fits, or the last where none does, laid out.
///
/// Every decision compares `room` with sums of the items' own widths, never
/// with a layout derived from it, so the fit depends on nothing but those
/// widths and the room. A wider room can only stop the narrowings earlier, and
/// each step takes one item one stand further from its widest, so widening the
/// row never stands an item narrower nor puts one out of it, and a filling is
/// never put out at all.
pub(crate) fn fit(items: &[Measure], room: Room) -> Fitted {
    let mut state = State::whole(items);
    for step in narrowings(items) {
        if state.taken(items, room) <= room.width {
            break;
        }
        state.take(step);
    }
    state.lay_out(items, room)
}

/// How far each item has narrowed.
#[derive(Clone, PartialEq, Debug)]
struct State {
    stands: Vec<Stand>,
    /// Whether the row has left out the overflow menu's trigger.
    triggerless: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stand {
    Shown(usize),
    Menu,
}

/// One step of the row narrowing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    Narrow(usize, Stand),
    LeaveTrigger,
}

/// The steps the row narrows by, in order.
///
/// Every group gives up its presentations one step at a time: each round
/// takes one step from every group still holding a narrower presentation, the
/// lowest priority first and, among equals, the one standing later in the
/// row. Only once no group can narrow further do groups move into the
/// overflow, in that same order, so that a control stays in the row, however
/// compact, for as long as one can. A row still too narrow then narrows each
/// filling to its floor, so that the menu's trigger, which reaches every group
/// given up before it, does not leave while a filling has room to give; and
/// last leaves out the trigger.
fn narrowings(items: &[Measure]) -> Vec<Step> {
    struct Group<'a> {
        at: usize,
        widths: &'a [Pixels],
        priority: Priority,
    }
    let mut order: Vec<Group> = (items.iter().enumerate().rev())
        .filter_map(|(at, item)| match &item.kind {
            Kind::Group {
                widths, priority, ..
            } => Some(Group {
                at,
                widths,
                priority: *priority,
            }),
            Kind::Filling { .. } => None,
        })
        .collect();
    order.sort_by_key(|group| group.priority);
    let presentations = order.iter().map(|group| group.widths.len());
    let mut steps = Vec::with_capacity(presentations.clone().sum::<usize>() + items.len() + 1);
    for stage in 1..presentations.max().unwrap_or(0) {
        steps.extend(
            (order.iter())
                .filter(|group| stage < group.widths.len())
                .map(|group| Step::Narrow(group.at, Stand::Shown(stage))),
        );
    }
    steps.extend((order.iter()).map(|group| Step::Narrow(group.at, Stand::Menu)));
    steps.extend(
        (items.iter().enumerate())
            .filter(
                |(_, item)| matches!(item.kind, Kind::Filling { least, floor } if floor < least),
            )
            .map(|(at, _)| Step::Narrow(at, Stand::Shown(1))),
    );
    steps.push(Step::LeaveTrigger);
    steps
}

/// An item standing in the row, before its place is known.
struct Standing {
    at: usize,
    stage: usize,
    /// Its width, taking in its rule, a filling's at its least.
    width: Pixels,
    ruled: bool,
    fills: bool,
    side: Side,
}

impl State {
    fn whole(items: &[Measure]) -> Self {
        Self {
            stands: vec![Stand::Shown(0); items.len()],
            triggerless: false,
        }
    }

    fn take(&mut self, step: Step) {
        match step {
            Step::Narrow(at, stand) => self.stands[at] = stand,
            Step::LeaveTrigger => self.triggerless = true,
        }
    }

    fn trigger(&self) -> bool {
        !self.triggerless && self.stands.contains(&Stand::Menu)
    }

    /// The items standing in the row, leading ones first, each ruled where it
    /// is parted from a control standing before it.
    fn standing(&self, items: &[Measure], room: Room) -> Vec<Standing> {
        let in_order = [Side::Leading, Side::Trailing]
            .into_iter()
            .flat_map(|side| {
                (items.iter().zip(&self.stands).enumerate())
                    .filter(move |(_, (item, _))| item.side == side)
            });
        let mut standing: Vec<Standing> = Vec::with_capacity(items.len());
        for (at, (item, stand)) in in_order {
            let Stand::Shown(stage) = *stand else {
                continue;
            };
            let (width, parted, fills) = match &item.kind {
                Kind::Group { widths, parted, .. } => (widths[stage], *parted, false),
                Kind::Filling { least, floor } => match stage {
                    0 => (*least, false, true),
                    _ => (*floor, false, true),
                },
            };
            let ruled = parted && !standing.is_empty();
            standing.push(Standing {
                at,
                stage,
                width: if ruled { width + room.rule } else { width },
                ruled,
                fills,
                side: item.side,
            });
        }
        standing
    }

    /// The width the row lays out at, its fillings at their least.
    fn taken(&self, items: &[Measure], room: Room) -> Pixels {
        let standing = self.standing(items, room);
        let trigger = self.trigger().then_some(room.overflow);
        spanned(
            standing.iter().map(|item| item.width).chain(trigger),
            room.gap,
        )
    }

    fn lay_out(&self, items: &[Measure], room: Room) -> Fitted {
        let standing = self.standing(items, room);
        let fillings = standing.iter().filter(|item| item.fills).count();
        let share = match fillings {
            0 => Pixels::ZERO,
            _ => (room.width - self.taken(items, room)) / fillings as f32,
        };
        let mut spots: Vec<Spot> = (self.stands.iter())
            .map(|stand| match stand {
                Stand::Menu if self.trigger() => Spot::Menu,
                _ => Spot::Out,
            })
            .collect();
        let mut place = |item: &Standing, x: Pixels| {
            let width = match item.fills {
                true => (item.width + share).max(Pixels::ZERO),
                false => item.width,
            };
            spots[item.at] = Spot::Row {
                stage: item.stage,
                x,
                width,
                ruled: item.ruled,
            };
            width
        };
        let (leading, trailing): (Vec<&Standing>, Vec<&Standing>) =
            standing.iter().partition(|item| item.side == Side::Leading);
        let mut x = Pixels::ZERO;
        for item in &leading {
            x += place(item, x) + room.gap;
        }
        let trigger = self.trigger().then(|| {
            let at = x;
            x += room.overflow + room.gap;
            at
        });
        let trailing_start = room.width - spanned(trailing.iter().map(|item| item.width), room.gap);
        let free_end = match trailing.is_empty() {
            true => room.width,
            false => trailing_start - room.gap,
        };
        let free = (x, free_end);
        let mut x = trailing_start;
        for item in &trailing {
            x += place(item, x) + room.gap;
        }
        Fitted {
            spots,
            trigger,
            free,
        }
    }
}

/// The width `widths` span side by side, `gap` apart.
fn spanned(widths: impl Iterator<Item = Pixels>, gap: Pixels) -> Pixels {
    let (count, sum) = widths.fold((0usize, Pixels::ZERO), |(count, sum), width| {
        (count + 1, sum + width)
    });
    sum + gap * count.saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::px;

    const GAP_WIDTH: f32 = 8.;
    const OVERFLOW_WIDTH: f32 = 28.;
    const RULE_WIDTH: f32 = 9.;

    fn room(width: f32) -> Room {
        Room {
            width: px(width),
            gap: px(GAP_WIDTH),
            overflow: px(OVERFLOW_WIDTH),
            rule: px(RULE_WIDTH),
        }
    }

    fn group(widths: &[f32], priority: Priority) -> Measure {
        Measure {
            kind: Kind::Group {
                widths: widths.iter().map(|&width| px(width)).collect(),
                priority,
                parted: false,
            },
            side: Side::Leading,
        }
    }

    fn filling(least: f32) -> Measure {
        folding(least, least)
    }

    /// A filling standing at `least` while groups give way, and narrowing to
    /// `floor` before the overflow menu's trigger leaves the row.
    fn folding(least: f32, floor: f32) -> Measure {
        Measure {
            kind: Kind::Filling {
                least: px(least),
                floor: px(floor),
            },
            side: Side::Leading,
        }
    }

    fn trailing(mut item: Measure) -> Measure {
        item.side = Side::Trailing;
        item
    }

    fn parted(mut item: Measure) -> Measure {
        if let Kind::Group { parted, .. } = &mut item.kind {
            *parted = true;
        }
        item
    }

    /// A row shaped as a graph's: a layout and a colour picker each dropping
    /// its label, the marks also collapsing, and panel toggles going icon-only.
    fn graph_row() -> Vec<Measure> {
        vec![
            group(&[150., 110.], 4),
            group(&[140., 100.], 3),
            group(&[260., 220., 110.], 2),
            trailing(group(&[300., 100.], 1)),
        ]
    }

    /// How an item stands, without where.
    #[derive(Clone, Copy, PartialEq, Debug)]
    enum Seen {
        Shown(usize),
        Menu,
        Out,
    }
    use Seen::{Menu, Out, Shown};

    fn seen(fitted: &Fitted) -> Vec<Seen> {
        (fitted.spots.iter())
            .map(|spot| match *spot {
                Spot::Row { stage, .. } => Shown(stage),
                Spot::Menu => Menu,
                Spot::Out => Out,
            })
            .collect()
    }

    fn stands(items: &[Measure], width: f32) -> Vec<Seen> {
        seen(&fit(items, room(width)))
    }

    /// Every state the row passes through as it narrows, the first standing
    /// whole.
    fn passes(items: &[Measure]) -> Vec<State> {
        let mut state = State::whole(items);
        let mut passed = vec![state.clone()];
        for step in narrowings(items) {
            state.take(step);
            passed.push(state.clone());
        }
        passed
    }

    #[test]
    fn a_row_with_room_for_everything_stands_whole() {
        let fitted = fit(&graph_row(), room(1000.));
        assert_eq!(seen(&fitted), [Shown(0); 4]);
        assert_eq!(fitted.trigger, None);
        let leading = 150. + 140. + 260. + GAP_WIDTH * 3.;
        assert_eq!(fitted.free, (px(leading), px(1000. - 300. - GAP_WIDTH)));
    }

    #[test]
    fn every_group_narrows_before_any_overflows() {
        use Stand::{Menu, Shown};
        let passed: Vec<(Vec<Stand>, bool)> = (passes(&graph_row()).into_iter())
            .map(|state| (state.stands, state.triggerless))
            .collect();
        assert_eq!(
            passed,
            [
                (vec![Shown(0), Shown(0), Shown(0), Shown(0)], false),
                (vec![Shown(0), Shown(0), Shown(0), Shown(1)], false),
                (vec![Shown(0), Shown(0), Shown(1), Shown(1)], false),
                (vec![Shown(0), Shown(1), Shown(1), Shown(1)], false),
                (vec![Shown(1), Shown(1), Shown(1), Shown(1)], false),
                (vec![Shown(1), Shown(1), Shown(2), Shown(1)], false),
                (vec![Shown(1), Shown(1), Shown(2), Menu], false),
                (vec![Shown(1), Shown(1), Menu, Menu], false),
                (vec![Shown(1), Menu, Menu, Menu], false),
                (vec![Menu, Menu, Menu, Menu], false),
                (vec![Menu, Menu, Menu, Menu], true),
            ]
        );
    }

    #[test]
    fn of_equal_priorities_the_later_group_gives_way_first() {
        let groups = [group(&[50., 20.], 1), group(&[50., 20.], 1)];
        assert_eq!(
            passes(&groups)[1].stands,
            [Stand::Shown(0), Stand::Shown(1)]
        );
    }

    /// The fit stops at the first stand that fits, and a row exactly as wide as
    /// that stand takes it.
    #[test]
    fn the_row_narrows_only_as_far_as_it_must() {
        let groups = graph_row();
        let whole = 150. + 140. + 260. + 300. + GAP_WIDTH * 3.;
        assert_eq!(stands(&groups, whole), [Shown(0); 4]);
        assert_eq!(
            stands(&groups, whole - 1.),
            [Shown(0), Shown(0), Shown(0), Shown(1)]
        );
    }

    #[test]
    fn the_overflow_trigger_takes_room_of_its_own() {
        let groups = graph_row();
        let all_narrowed = 110. + 100. + 110. + 100. + GAP_WIDTH * 3.;
        let fitted = fit(&groups, room(all_narrowed - 1.));
        assert_eq!(seen(&fitted), [Shown(1), Shown(1), Shown(2), Menu]);
        assert_eq!(
            fitted.trigger,
            Some(px(110. + 100. + 110. + GAP_WIDTH * 3.))
        );
    }

    /// The trigger reaches every group the row has given up, so it is the last
    /// thing a row too narrow for its narrowest stand gives up in turn.
    #[test]
    fn a_row_too_narrow_for_the_trigger_leaves_every_group_out() {
        let groups = graph_row();
        let fitted = fit(&groups, room(OVERFLOW_WIDTH));
        assert_eq!(seen(&fitted), [Menu; 4]);
        assert_eq!(fitted.trigger, Some(Pixels::ZERO));
        let fitted = fit(&groups, room(OVERFLOW_WIDTH - 1.));
        assert_eq!(seen(&fitted), [Out; 4]);
        assert_eq!(fitted.trigger, None);
    }

    #[test]
    fn a_rule_takes_room_only_after_a_control() {
        let items = [group(&[50.], 1), parted(group(&[60.], 2))];
        let whole = 50. + GAP_WIDTH + RULE_WIDTH + 60.;
        let fitted = fit(&items, room(whole));
        assert_eq!(
            fitted.spots[1],
            Spot::Row {
                stage: 0,
                x: px(50. + GAP_WIDTH),
                width: px(RULE_WIDTH + 60.),
                ruled: true,
            }
        );
        let fitted = fit(&items, room(whole - 1.));
        assert_eq!(seen(&fitted), [Menu, Shown(0)]);
        assert!(matches!(fitted.spots[1], Spot::Row { ruled: false, .. }));
    }

    /// An item is reached at its own span, short of its rule, while it stands
    /// in the row, at the trigger's while it stands in the menu, and nowhere
    /// once the trigger has left the row too.
    #[test]
    fn an_item_is_reached_where_it_stands() {
        let items = [group(&[50.], 2), parted(group(&[60.], 1))];
        let whole = 50. + GAP_WIDTH + RULE_WIDTH + 60.;
        let fitted = fit(&items, room(whole));
        assert_eq!(
            fitted.reached_at(1, room(whole)),
            Some((px(50. + GAP_WIDTH + RULE_WIDTH), px(60.)))
        );
        let narrower = room(whole - 1.);
        let fitted = fit(&items, narrower);
        assert_eq!(seen(&fitted), [Shown(0), Menu]);
        assert_eq!(
            fitted.reached_at(1, narrower),
            Some((px(50. + GAP_WIDTH), px(OVERFLOW_WIDTH)))
        );
        let too_narrow = room(OVERFLOW_WIDTH - 1.);
        let fitted = fit(&items, too_narrow);
        assert_eq!(fitted.reached_at(0, too_narrow), None);
        assert_eq!(fitted.reached_at(1, too_narrow), None);
    }

    /// A filling stands at its least while any group stands in the row: every
    /// group gives way first.
    #[test]
    fn a_filling_holds_its_least_while_groups_give_way() {
        const FIELD_LEAST: f32 = 240.;
        let items = [group(&[90.], 1), filling(FIELD_LEAST)];
        let both = 90. + FIELD_LEAST + GAP_WIDTH;
        assert_eq!(stands(&items, both), [Shown(0), Shown(0)]);
        let narrower = both - 1.;
        let fitted = fit(&items, room(narrower));
        assert_eq!(seen(&fitted), [Menu, Shown(0)]);
        let field = narrower - GAP_WIDTH - OVERFLOW_WIDTH;
        assert_eq!(
            fitted.spots[1],
            Spot::Row {
                stage: 0,
                x: Pixels::ZERO,
                width: px(field),
                ruled: false,
            }
        );
        assert_eq!(fitted.trigger, Some(px(field + GAP_WIDTH)));
    }

    /// Below room for its least beside the trigger, a filling stays in the
    /// row, taking all of it.
    #[test]
    fn a_filling_too_narrow_for_its_least_takes_the_whole_row() {
        let items = [group(&[90.], 1), filling(240.)];
        let fitted = fit(&items, room(100.));
        assert_eq!(seen(&fitted), [Out, Shown(0)]);
        assert_eq!(fitted.trigger, None);
        assert!(matches!(fitted.spots[1], Spot::Row { width, .. } if width == px(100.)));
    }

    /// A filling able to stand narrower than its least does so before the
    /// row leaves out the trigger reaching the groups it gave up, and takes
    /// the whole row only once even its floor leaves no room for the trigger.
    #[test]
    fn a_folding_filling_keeps_the_trigger_in_the_row() {
        const LEAST: f32 = 420.;
        const FLOOR: f32 = 120.;
        let items = [group(&[90.], 1), folding(LEAST, FLOOR)];
        let beside_trigger = |least: f32| least + GAP_WIDTH + OVERFLOW_WIDTH;
        assert_eq!(stands(&items, beside_trigger(LEAST)), [Menu, Shown(0)]);
        let folded = fit(&items, room(beside_trigger(LEAST) - 1.));
        assert_eq!(seen(&folded), [Menu, Shown(1)]);
        assert_eq!(
            folded.trigger,
            Some(px(beside_trigger(LEAST) - 1. - OVERFLOW_WIDTH))
        );
        assert_eq!(stands(&items, beside_trigger(FLOOR)), [Menu, Shown(1)]);
        let whole = fit(&items, room(beside_trigger(FLOOR) - 1.));
        assert_eq!(seen(&whole), [Out, Shown(1)]);
        assert_eq!(whole.trigger, None);
    }

    /// A trailing group moves into the menu, with every other group, before a
    /// filling narrows below its least: a control in the menu stays in reach.
    #[test]
    fn every_group_moves_into_the_menu_before_a_filling_narrows() {
        const LEAST: f32 = 300.;
        const FLOOR: f32 = 120.;
        const LEGEND: f32 = 30.;
        let items = [
            group(&[90.], 1),
            folding(LEAST, FLOOR),
            trailing(group(&[LEGEND], 0)),
        ];
        let beside_trigger = |filling: f32| filling + OVERFLOW_WIDTH + GAP_WIDTH;
        let legend_out = beside_trigger(LEAST) + 90. + GAP_WIDTH;
        assert_eq!(stands(&items, legend_out), [Shown(0), Shown(0), Menu]);
        assert_eq!(
            stands(&items, beside_trigger(LEAST)),
            [Menu, Shown(0), Menu]
        );
        assert_eq!(
            stands(&items, beside_trigger(LEAST) - 1.),
            [Menu, Shown(1), Menu]
        );
    }

    /// A graph's query row as a layout at a fractional scale measured it: the
    /// boxes menu and the query field filling the rest, standing a little way
    /// into the window. The field stays in the row at every width, at its
    /// least wherever there is room for it beside the trigger.
    #[test]
    fn a_query_field_stays_in_its_row_at_every_width() {
        const SCALE: f32 = 1.25;
        const BOXES_DEVICE: f32 = 122.;
        const FIELD_DEVICE: f32 = 326.;
        const MOST_DEVICE: usize = 1000;
        let least = FIELD_DEVICE / SCALE;
        let items = [group(&[BOXES_DEVICE / SCALE], 1), filling(least)];
        for device in 0..MOST_DEVICE {
            let width = device as f32 / SCALE;
            let fitted = fit(&items, room(width));
            let Spot::Row { width: laid, .. } = fitted.spots[1] else {
                panic!("the field left the row at {width}px");
            };
            if width >= least + GAP_WIDTH + OVERFLOW_WIDTH {
                assert!(
                    laid >= px(least),
                    "the field stood below its least at {width}px"
                );
            }
        }
    }

    /// A deterministic source of test rows.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }

        /// A width below `most` device pixels, whole in device pixels at
        /// `scale` as a layout rounds it.
        fn width(&mut self, scale: f32, most: u64) -> f32 {
            self.below(most) as f32 / scale
        }
    }

    /// The device scales a generated row's widths are whole at.
    const SCALES: [f32; 4] = [1., 1.25, 1.5, 2.];

    /// A row of groups of every kind a toolbar declares, on either side, and
    /// at most one filling.
    fn generated(rng: &mut Rng, scale: f32) -> Vec<Measure> {
        const MOST_GROUPS: u64 = 6;
        const MOST_PRESENTATIONS: u64 = 3;
        const MOST_PRIORITY: u64 = 4;
        const MOST_DEVICE_WIDTH: u64 = 400;
        let mut items: Vec<Measure> = (0..=rng.below(MOST_GROUPS))
            .map(|_| {
                let mut widths: Vec<Pixels> = (0..=rng.below(MOST_PRESENTATIONS))
                    .map(|_| px(rng.width(scale, MOST_DEVICE_WIDTH) + 1.))
                    .collect();
                widths.sort_by(|a, b| f32::from(*b).total_cmp(&f32::from(*a)));
                Measure {
                    kind: Kind::Group {
                        widths,
                        priority: rng.below(MOST_PRIORITY) as Priority,
                        parted: rng.below(2) == 0,
                    },
                    side: match rng.below(3) {
                        0 => Side::Trailing,
                        _ => Side::Leading,
                    },
                }
            })
            .collect();
        if rng.below(2) == 0 {
            let at = rng.below(items.len() as u64 + 1) as usize;
            let least = rng.width(scale, MOST_DEVICE_WIDTH);
            let floor = rng.width(scale, MOST_DEVICE_WIDTH).min(least);
            items.insert(at, folding(least, floor));
        }
        items
    }

    /// How far `spot` stands from the item's widest presentation.
    fn narrowed(spot: Spot) -> usize {
        match spot {
            Spot::Row { stage, .. } => stage,
            Spot::Menu => usize::MAX - 1,
            Spot::Out => usize::MAX,
        }
    }

    /// Widening a row a device pixel at a time, from nothing to past its whole
    /// width, never stands an item narrower nor puts one out, never puts a
    /// filling out at all, and lays everything out within the row; and every
    /// width stands the row the same whichever width came before it.
    #[test]
    fn a_wider_row_never_stands_an_item_narrower() {
        const ROWS: usize = 400;
        const PAST_WHOLE: f32 = 40.;
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        for row in 0..ROWS {
            let scale = SCALES[row % SCALES.len()];
            let items = generated(&mut rng, scale);
            let whole = f32::from(State::whole(&items).taken(&items, room(0.))) + PAST_WHOLE;
            let widths: Vec<f32> = (0..=(whole * scale) as usize)
                .map(|device| device as f32 / scale)
                .collect();
            let widening: Vec<Fitted> = widths
                .iter()
                .map(|&width| fit(&items, room(width)))
                .collect();
            let narrowing: Vec<Fitted> = (widths.iter().rev())
                .map(|&width| fit(&items, room(width)))
                .collect();
            assert!(
                widening.iter().eq(narrowing.iter().rev()),
                "row {row} stood by the way it was resized"
            );
            for (at, item) in items.iter().enumerate() {
                if let Kind::Filling { .. } = item.kind {
                    assert!(
                        (widening.iter())
                            .all(|fitted| matches!(fitted.spots[at], Spot::Row { .. })),
                        "row {row} put its filling out"
                    );
                }
            }
            for (pair, width) in widening.windows(2).zip(&widths[1..]) {
                for at in 0..items.len() {
                    assert!(
                        narrowed(pair[1].spots[at]) <= narrowed(pair[0].spots[at]),
                        "row {row} narrowed item {at} widening to {width}px"
                    );
                }
            }
            for (fitted, &width) in widening.iter().zip(&widths) {
                assert_side_by_side_within(fitted, width, row);
            }
        }
    }

    /// The items `fitted` stands in the row, and its trigger, lie apart from
    /// one another within `width`.
    fn assert_side_by_side_within(fitted: &Fitted, width: f32, row: usize) {
        // The rounding of sums of a few widths.
        const SLACK: f32 = 1e-3;
        let mut spans: Vec<(f32, f32)> = (fitted.spots.iter())
            .filter_map(|spot| match *spot {
                Spot::Row { x, width, .. } => Some((f32::from(x), f32::from(x + width))),
                _ => None,
            })
            .chain(
                fitted
                    .trigger
                    .map(|x| (f32::from(x), f32::from(x) + OVERFLOW_WIDTH)),
            )
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        if let (Some(first), Some(last)) = (spans.first(), spans.last()) {
            assert!(
                first.0 >= -SLACK && last.1 <= width + SLACK,
                "row {row} overran {width}px: {spans:?}"
            );
        }
        for pair in spans.windows(2) {
            assert!(
                pair[0].1 + GAP_WIDTH <= pair[1].0 + SLACK,
                "row {row} overlapped at {width}px: {spans:?}"
            );
        }
    }
}
