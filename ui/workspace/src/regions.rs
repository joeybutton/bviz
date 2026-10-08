use crate::PanelGroup;
use gpui::Axis;

/// What a view is to the window showing it, which decides the region of the
/// window it opens in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// A drawing of the document's structure: its graph, or a chart of it.
    Canvas,
    /// The document laid out over time.
    Timeline,
    /// Text read line by line.
    Log,
    /// Figures aggregated over the document.
    Analysis,
}

/// The share of the window's width the column of the canvas over the log
/// takes. One divider parts it from the timeline over the analysis, so
/// dragging it resizes the timeline and the analysis together.
const LEFT_SHARE: f32 = 1.0 / 3.0;
/// The share of its column's height the upper region takes, the canvas's on
/// the left and the timeline's on the right.
const TOP_SHARE: f32 = 2.0 / 3.0;

/// Panels gathered by role into the regions a window opens with, each region's
/// in the order pushed. Built around a leading panel and only ever grown, so it
/// always holds one.
pub(crate) struct Regions<T> {
    /// The panel the regions were built around, ahead of every other panel of
    /// its role.
    leading: (Role, T),
    others: ByRole<T>,
}

/// A list of panels for each role.
struct ByRole<T> {
    canvas: Vec<T>,
    timeline: Vec<T>,
    log: Vec<T>,
    analysis: Vec<T>,
}

impl<T> ByRole<T> {
    fn of(&mut self, role: Role) -> &mut Vec<T> {
        match role {
            Role::Canvas => &mut self.canvas,
            Role::Timeline => &mut self.timeline,
            Role::Log => &mut self.log,
            Role::Analysis => &mut self.analysis,
        }
    }
}

impl<T> Regions<T> {
    pub(crate) fn new(role: Role, leading: T) -> Self {
        Self {
            leading: (role, leading),
            others: ByRole {
                canvas: Vec::new(),
                timeline: Vec::new(),
                log: Vec::new(),
                analysis: Vec::new(),
            },
        }
    }

    /// The panel the regions were built around, which the pane holding it shows
    /// first.
    pub(crate) fn leading(&self) -> &T {
        &self.leading.1
    }

    /// The regions of `panels`, `None` where there are none.
    pub(crate) fn gather(panels: impl IntoIterator<Item = (Role, T)>) -> Option<Self> {
        let mut panels = panels.into_iter();
        let (role, first) = panels.next()?;
        let mut regions = Self::new(role, first);
        regions.extend(panels);
        Some(regions)
    }

    pub(crate) fn push(&mut self, role: Role, panel: T) {
        self.others.of(role).push(panel);
    }

    /// The arrangement of the regions holding panels.
    pub(crate) fn arrange(self) -> PanelGroup<T> {
        let Self {
            leading: (role, leading),
            others: mut regions,
        } = self;
        regions.of(role).insert(0, leading);
        let region = |panels: Vec<T>| {
            let mut panels = panels.into_iter().map(Some);
            match panels.next() {
                Some(first) => PanelGroup::tabs(first, panels.collect()),
                None => PanelGroup::one(None),
            }
        };
        PanelGroup::split(
            Axis::Horizontal,
            LEFT_SHARE,
            PanelGroup::split(
                Axis::Vertical,
                TOP_SHARE,
                region(regions.canvas),
                region(regions.log),
            ),
            PanelGroup::split(
                Axis::Vertical,
                TOP_SHARE,
                region(regions.timeline),
                region(regions.analysis),
            ),
        )
        .compact()
        .expect("regions hold a panel")
    }
}

impl<T> Extend<(Role, T)> for Regions<T> {
    fn extend<I: IntoIterator<Item = (Role, T)>>(&mut self, panels: I) {
        for (role, panel) in panels {
            self.push(role, panel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::shape;
    use crate::testing::{Extent, arrange, assert_extents};

    fn arranged(panels: &[(Role, char)]) -> crate::Layout<String> {
        let regions = Regions::gather(panels.iter().copied()).expect("panels to arrange");
        arrange(regions.arrange())
    }

    const CANVAS: (Role, char) = (Role::Canvas, 'c');
    const TIMELINE: (Role, char) = (Role::Timeline, 't');
    const LOG: (Role, char) = (Role::Log, 'l');
    const ANALYSIS: (Role, char) = (Role::Analysis, 'a');

    /// Panels pushed by role, the shape they arrange into, and each pane's
    /// extent.
    type Case<'a> = (&'a [(Role, char)], &'a str, &'a [Extent<'a>]);

    const TOP: f32 = 2.0 / 3.0;
    const BOTTOM: f32 = 1.0 / 3.0;
    const LEFT: f32 = 1.0 / 3.0;
    const RIGHT: f32 = 2.0 / 3.0;

    #[test]
    fn every_role_takes_its_region_of_the_template() {
        let layout = arranged(&[ANALYSIS, LOG, TIMELINE, CANVAS]);
        assert_eq!(shape(&layout), "((c/l)|(t/a))");
        assert_extents(
            &layout,
            &[
                ("c", LEFT, TOP),
                ("l", LEFT, BOTTOM),
                ("t", RIGHT, TOP),
                ("a", RIGHT, BOTTOM),
            ],
        );
    }

    /// Every split of the template rests at the shares it lays out: a divider
    /// dragged away and put back to rest returns to its share.
    #[test]
    fn the_template_rests_at_its_shares() {
        let mut layout = arranged(&[CANVAS, LOG, TIMELINE, ANALYSIS]);
        let crate::Layout::Split(columns) = &layout else {
            panic!("the template is a split");
        };
        let mut dividers: Vec<_> = columns
            .sides()
            .iter()
            .filter_map(|side| side.divider)
            .collect();
        for column in columns.sides() {
            if let crate::Layout::Split(regions) = column.layout {
                dividers.extend(regions.sides().iter().filter_map(|side| side.divider));
            }
        }
        assert_eq!(dividers.len(), 3);
        for divider in dividers {
            assert!(layout.move_divider(divider, 0.5));
            assert!(layout.rest_split(divider));
        }
        assert_extents(
            &layout,
            &[
                ("c", LEFT, TOP),
                ("l", LEFT, BOTTOM),
                ("t", RIGHT, TOP),
                ("a", RIGHT, BOTTOM),
            ],
        );
    }

    /// A region holding no panel takes no place: the region above or below it
    /// takes its height, and a column left empty gives its width to the other.
    #[test]
    fn the_regions_left_divide_the_window_between_them() {
        let cases: [Case; 14] = [
            (&[CANVAS], "c", &[("c", 1.0, 1.0)]),
            (&[TIMELINE], "t", &[("t", 1.0, 1.0)]),
            (&[LOG], "l", &[("l", 1.0, 1.0)]),
            (&[ANALYSIS], "a", &[("a", 1.0, 1.0)]),
            (
                &[CANVAS, TIMELINE],
                "(c|t)",
                &[("c", LEFT, 1.0), ("t", RIGHT, 1.0)],
            ),
            (
                &[LOG, ANALYSIS],
                "(l|a)",
                &[("l", LEFT, 1.0), ("a", RIGHT, 1.0)],
            ),
            (
                &[CANVAS, LOG],
                "(c/l)",
                &[("c", 1.0, TOP), ("l", 1.0, BOTTOM)],
            ),
            (
                &[CANVAS, ANALYSIS],
                "(c|a)",
                &[("c", LEFT, 1.0), ("a", RIGHT, 1.0)],
            ),
            (
                &[TIMELINE, LOG],
                "(l|t)",
                &[("l", LEFT, 1.0), ("t", RIGHT, 1.0)],
            ),
            (
                &[TIMELINE, ANALYSIS],
                "(t/a)",
                &[("t", 1.0, TOP), ("a", 1.0, BOTTOM)],
            ),
            (
                &[CANVAS, TIMELINE, LOG],
                "((c/l)|t)",
                &[("c", LEFT, TOP), ("l", LEFT, BOTTOM), ("t", RIGHT, 1.0)],
            ),
            (
                &[CANVAS, TIMELINE, ANALYSIS],
                "(c|(t/a))",
                &[("c", LEFT, 1.0), ("t", RIGHT, TOP), ("a", RIGHT, BOTTOM)],
            ),
            (
                &[CANVAS, LOG, ANALYSIS],
                "((c/l)|a)",
                &[("c", LEFT, TOP), ("l", LEFT, BOTTOM), ("a", RIGHT, 1.0)],
            ),
            (
                &[TIMELINE, LOG, ANALYSIS],
                "(l|(t/a))",
                &[("l", LEFT, 1.0), ("t", RIGHT, TOP), ("a", RIGHT, BOTTOM)],
            ),
        ];
        for (panels, want_shape, want_extents) in cases {
            let layout = arranged(panels);
            assert_eq!(shape(&layout), want_shape);
            assert_extents(&layout, want_extents);
        }
    }

    /// Several panels of one role share its region's pane as tabs, never
    /// panes of their own.
    #[test]
    fn panels_of_one_role_are_tabs_of_its_region_in_the_order_pushed() {
        let layout = arranged(&[(Role::Canvas, 'b'), (Role::Canvas, 'a')]);
        assert_eq!(shape(&layout), "ba");
    }

    /// Two profiles, each a timeline and three analyses: pushes interleaved
    /// across roles keep each region's own order.
    #[test]
    fn interleaved_pushes_keep_each_regions_order() {
        let profile = |timeline, analyses: [char; 3]| {
            let mut panels = vec![(Role::Timeline, timeline)];
            panels.extend(analyses.map(|panel| (Role::Analysis, panel)));
            panels
        };
        let mut panels = profile('a', ['b', 'c', 'd']);
        panels.extend(profile('e', ['f', 'g', 'h']));
        assert_eq!(shape(&arranged(&panels)), "(ae/bcdfgh)");
    }

    /// The panel the regions were built around leads its region's pane
    /// wherever the layout places that pane, so a window built around a
    /// profile opens on its timeline even beside a log laid out ahead of it.
    #[test]
    fn the_leading_panel_leads_its_regions_pane() {
        let regions =
            Regions::gather([(Role::Timeline, 't'), LOG, (Role::Timeline, 'u')]).expect("panels");
        assert_eq!(*regions.leading(), 't');
        assert_eq!(shape(&arrange(regions.arrange())), "(l|tu)");
    }

    #[test]
    fn no_panels_gather_into_no_regions() {
        assert!(Regions::<char>::gather([]).is_none());
    }
}
