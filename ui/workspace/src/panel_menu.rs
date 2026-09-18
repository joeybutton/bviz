//! The menu of rearrangements a panel offers: what it offers where the panel
//! stands, and the rows a [`Menu`] puts that up as.
//!
//! [`Menu`]: bviz_ui_components::Menu

use crate::workspace::{Destination, Workspace, re_homes};
use crate::{Arrangement, Pane, Side};
use bviz_ui_components::{MenuItem, SubmenuItem};
use gpui::{App, Entity, EntityId, SharedString, WeakEntity, Window};

/// The sides a panel can be split off towards or moved to, in the order a menu
/// offers them.
const SIDES: [Side; 4] = [Side::Left, Side::Right, Side::Top, Side::Bottom];

/// One rearrangement a panel's menu offers, `W` naming a window a move can take
/// the panel to.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum Offer<W> {
    /// A pane of its own on that side of the pane holding it.
    Split(Side),
    /// A pane of its own spanning that edge of the window.
    Edge(Side),
    NewWindow,
    Window(W),
    /// Draw the pane holding it in place of the whole window's body.
    Maximize,
    /// Put every pane back where the arrangement lays it out.
    Restore,
    EvenSplit,
    Close,
}

/// One row of a panel's menu, as [`offered`] works it out.
#[derive(PartialEq, Eq, Debug)]
pub(crate) struct Row<W> {
    pub label: SharedString,
    pub opens: Opens<W>,
}

/// What a row of a panel's menu stands for.
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum Opens<W> {
    Offer(Offer<W>),
    Submenu(Vec<Under<W>>),
}

/// One row of the submenu a row opens beside itself. Every such row stands for
/// an offer of its own, a menu opening no list beside a submenu.
#[derive(PartialEq, Eq, Debug)]
pub(crate) struct Under<W> {
    pub label: SharedString,
    pub offer: Offer<W>,
}

/// Where a panel stands, as far as what its menu can offer goes.
pub(crate) struct Standing<'a, P = Entity<Pane>> {
    pub arrangement: &'a Arrangement<P>,
    /// The pane holding the panel.
    pub pane: &'a P,
    /// Whether that pane holds the panel and no other.
    pub alone: bool,
}

/// What choosing a row of a panel's menu does to the panel it was opened for.
/// The pane and the window are type parameters, as [`Destination`]'s pane is.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Deed<P = Entity<Pane>, W = WeakEntity<Workspace>> {
    /// Re-homes the panel.
    Move(Destination<P>),
    /// Moves the panel into the window that workspace frames.
    ToWindow(W),
    /// Draws the pane holding the panel in place of the whole window's body, or
    /// puts every pane back where one is drawn in place of them already.
    Maximize,
    /// Evens the split laying the pane holding the panel out.
    EvenSplit,
    Close,
}

/// What the menu of a panel standing at `standing` offers, `windows` naming the
/// session's other open windows. A rearrangement that would leave the window
/// laid out as it already is ([`re_homes`]) is left out rather than offered and
/// refused, as is one there is nothing in this window to carry out.
pub(crate) fn offered<P: Clone + PartialEq, W: Clone>(
    standing: &Standing<'_, P>,
    windows: &[(W, SharedString)],
) -> Vec<Row<W>> {
    let Standing {
        arrangement,
        pane,
        alone,
    } = *standing;
    let layout = arrangement.layout();
    let mut rows = Vec::new();
    for side in SIDES {
        let to = Destination::Beside {
            pane: pane.clone(),
            side,
        };
        if re_homes(&to, pane, alone, layout) {
            rows.push(offer(
                format!("Split {}", split_name(side)),
                Offer::Split(side),
            ));
        }
    }
    let edges: Vec<Under<W>> = SIDES
        .into_iter()
        .filter(|&side| re_homes(&Destination::WindowEdge(side), pane, alone, layout))
        .map(|side| Under {
            label: edge_name(side).into(),
            offer: Offer::Edge(side),
        })
        .collect();
    if !edges.is_empty() {
        rows.push(Row {
            label: "Move to Window Edge".into(),
            opens: Opens::Submenu(edges),
        });
    }
    rows.push(offer("Move to New Window", Offer::NewWindow));
    if !windows.is_empty() {
        rows.push(Row {
            label: "Move to Window".into(),
            opens: Opens::Submenu(
                windows
                    .iter()
                    .map(|(window, title)| Under {
                        label: title.clone(),
                        offer: Offer::Window(window.clone()),
                    })
                    .collect(),
            ),
        });
    }
    if arrangement.maximized().is_some() {
        rows.push(offer("Restore", Offer::Restore));
    } else if arrangement.maximizes(pane) {
        rows.push(offer("Maximize", Offer::Maximize));
    }
    if layout.innermost_divider(pane).is_some() {
        rows.push(offer("Even Out Split", Offer::EvenSplit));
    }
    rows.push(offer("Close", Offer::Close));
    rows
}

/// The menu `standing` offers for `panel`, as rows a [`Menu`] puts up: choosing
/// one asks `workspace` to carry it out.
///
/// [`Menu`]: bviz_ui_components::Menu
pub(crate) fn items(
    panel: EntityId,
    workspace: &WeakEntity<Workspace>,
    standing: &Standing<'_>,
    windows: &[(WeakEntity<Workspace>, SharedString)],
) -> Vec<MenuItem> {
    let offered = offered(standing, windows);
    let mut items = Vec::with_capacity(offered.len());
    for row in offered {
        items.push(match row.opens {
            Opens::Submenu(under) => MenuItem::submenu(
                row.label,
                under
                    .into_iter()
                    .map(|under| {
                        SubmenuItem::new(
                            under.label,
                            taking(under.offer, panel, workspace, standing.pane),
                        )
                    })
                    .collect(),
            ),
            Opens::Offer(offer) => {
                MenuItem::new(row.label, taking(offer, panel, workspace, standing.pane))
            }
        });
    }
    items
}

/// What choosing the row `offer` stands for does: asks `workspace` to carry out
/// what that offer comes to for `panel`.
fn taking(
    offer: Offer<WeakEntity<Workspace>>,
    panel: EntityId,
    workspace: &WeakEntity<Workspace>,
    pane: &Entity<Pane>,
) -> impl Fn(&mut Window, &mut App) + use<> {
    let deed = deed(offer, pane);
    let workspace = workspace.clone();
    move |window, cx| {
        let deed = deed.clone();
        workspace
            .update(cx, |workspace, cx| {
                workspace.take_deed(panel, deed, window, cx);
            })
            .ok();
    }
}

/// What carrying `offer` out for a panel held by `pane` comes to.
fn deed<P: Clone, W>(offer: Offer<W>, pane: &P) -> Deed<P, W> {
    match offer {
        Offer::Split(side) => Deed::Move(Destination::Beside {
            pane: pane.clone(),
            side,
        }),
        Offer::Edge(side) => Deed::Move(Destination::WindowEdge(side)),
        Offer::NewWindow => Deed::Move(Destination::NewWindow),
        Offer::Window(workspace) => Deed::ToWindow(workspace),
        // Asking of a window that draws a pane in place of the rest puts them
        // all back, whichever pane asks, so both read as the one deed.
        Offer::Maximize | Offer::Restore => Deed::Maximize,
        Offer::EvenSplit => Deed::EvenSplit,
        Offer::Close => Deed::Close,
    }
}

fn offer<W>(label: impl Into<SharedString>, offer: Offer<W>) -> Row<W> {
    Row {
        label: label.into(),
        opens: Opens::Offer(offer),
    }
}

/// How a side reads where a pane split off towards it is offered.
fn split_name(side: Side) -> &'static str {
    match side {
        Side::Left => "Left",
        Side::Right => "Right",
        Side::Top => "Up",
        Side::Bottom => "Down",
    }
}

/// How a side reads where it names an edge of the window.
fn edge_name(side: Side) -> &'static str {
    match side {
        Side::Left => "Left",
        Side::Right => "Right",
        Side::Top => "Top",
        Side::Bottom => "Bottom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layout, Maximize};
    use gpui::Axis;

    /// A window naming itself, as the menu of another window's panel lists it.
    type Window = &'static str;

    /// The other windows a session holds open, each named by its own title.
    fn windows<const N: usize>(titles: [Window; N]) -> Vec<(Window, SharedString)> {
        titles.map(|title| (title, title.into())).to_vec()
    }

    /// What the menu of a panel held by `pane` offers, that pane holding it
    /// `alone` or among others, out of an arrangement of plain panes and with
    /// no other window open.
    fn offers(arrangement: &Arrangement<char>, pane: char, alone: bool) -> Vec<Row<Window>> {
        offered(&standing(arrangement, &pane, alone), &[])
    }

    fn standing<'a>(
        arrangement: &'a Arrangement<char>,
        pane: &'a char,
        alone: bool,
    ) -> Standing<'a, char> {
        Standing {
            arrangement,
            pane,
            alone,
        }
    }

    /// How each row of a menu reads, a row opening a submenu followed by the
    /// rows it opens, each set in from it.
    fn labels<W>(rows: &[Row<W>]) -> Vec<String> {
        let mut read = Vec::with_capacity(rows.len());
        for row in rows {
            read.push(row.label.to_string());
            if let Opens::Submenu(under) = &row.opens {
                read.extend(under.iter().map(|under| format!("  {}", under.label)));
            }
        }
        read
    }

    /// The rows a menu opens beside the row reading `label`.
    fn under<'a, W>(rows: &'a [Row<W>], label: &str) -> &'a [Under<W>] {
        match &rows
            .iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("{label} is offered"))
            .opens
        {
            Opens::Submenu(under) => under,
            Opens::Offer(_) => panic!("{label} opens rows of its own"),
        }
    }

    /// Panes 'a' and 'b' side by side.
    fn split() -> Arrangement<char> {
        Arrangement::new(Layout::split_at(
            Axis::Horizontal,
            0.5,
            Layout::Pane('a'),
            Layout::Pane('b'),
        ))
    }

    /// Pane 'a' beside 'b' above 'c', so that a pane is laid out against some
    /// of the window's edges and away from the rest.
    fn nested() -> Arrangement<char> {
        Arrangement::new(Layout::split_at(
            Axis::Horizontal,
            0.5,
            Layout::Pane('a'),
            Layout::split_at(Axis::Vertical, 0.5, Layout::Pane('b'), Layout::Pane('c')),
        ))
    }

    /// A window laying out one pane holding one panel is that panel: it stands
    /// over the whole of the window already, so every place a rearrangement
    /// could put it is the place it stands — every place but a window of its
    /// own.
    #[test]
    fn a_lone_panel_of_a_lone_pane_is_offered_only_what_would_move_it() {
        let arrangement = Arrangement::new(Layout::Pane('a'));
        assert_eq!(
            labels(&offers(&arrangement, 'a', true)),
            ["Move to New Window", "Close"]
        );
    }

    /// A pane holding several panels can give one of them a pane of its own,
    /// wherever in the window that pane goes.
    #[test]
    fn a_panel_beside_others_in_its_pane_is_offered_every_split() {
        let arrangement = Arrangement::new(Layout::Pane('a'));
        assert_eq!(
            labels(&offers(&arrangement, 'a', false)),
            [
                "Split Left",
                "Split Right",
                "Split Up",
                "Split Down",
                "Move to Window Edge",
                "  Left",
                "  Right",
                "  Top",
                "  Bottom",
                "Move to New Window",
                "Close",
            ]
        );
    }

    /// A lone panel of a pane laid out beside another is offered the edges of
    /// the window its pane does not already span, the pane over the whole body,
    /// and an even share of the split laying it out.
    #[test]
    fn a_lone_panel_in_a_split_is_offered_the_edges_its_pane_does_not_span() {
        assert_eq!(
            labels(&offers(&split(), 'a', true)),
            [
                "Move to Window Edge",
                "  Right",
                "  Top",
                "  Bottom",
                "Move to New Window",
                "Maximize",
                "Even Out Split",
                "Close",
            ]
        );
    }

    /// A panel among others in a pane that is itself laid out beside another is
    /// the one standing offered every rearrangement at once: a pane of its own
    /// on any side, any edge of the window, the body, and an even split.
    #[test]
    fn a_panel_among_others_in_a_split_pane_is_offered_everything() {
        assert_eq!(
            labels(&offers(&split(), 'a', false)),
            [
                "Split Left",
                "Split Right",
                "Split Up",
                "Split Down",
                "Move to Window Edge",
                "  Left",
                "  Right",
                "  Top",
                "  Bottom",
                "Move to New Window",
                "Maximize",
                "Even Out Split",
                "Close",
            ]
        );
    }

    /// The pane already drawn in place of the whole window's body is offered
    /// the panes back rather than the body it has.
    #[test]
    fn a_maximized_pane_is_offered_the_panes_it_hides_back() {
        let mut arrangement = split();
        assert_eq!(arrangement.toggle_maximize(&'a'), Maximize::Maximized);
        let read = labels(&offers(&arrangement, 'a', true));
        assert!(read.contains(&"Restore".to_string()), "{read:?}");
        assert!(!read.contains(&"Maximize".to_string()), "{read:?}");
    }

    /// The body is offered exactly where asking for it would be carried out, so
    /// no row is offered that the arrangement would refuse.
    #[test]
    fn the_body_is_offered_only_where_the_arrangement_would_give_it() {
        for arrangement in [Arrangement::new(Layout::Pane('a')), split(), nested()] {
            for pane in ['a', 'b', 'z'] {
                let offered = offers(&arrangement, pane, false);
                let maximizes = arrangement.maximizes(&pane);
                assert_eq!(
                    labels(&offered).contains(&"Maximize".to_string()),
                    maximizes,
                    "{pane}"
                );
            }
        }
    }

    /// Every side a split or an edge is offered for is one a move would be
    /// carried out towards, and every side left out is one it would refuse: a
    /// row that does nothing is worse than no row at all.
    #[test]
    fn every_offer_is_one_the_move_would_carry_out() {
        for arrangement in [Arrangement::new(Layout::Pane('a')), split(), nested()] {
            for pane in ['a', 'b', 'c'] {
                for alone in [true, false] {
                    let layout = arrangement.layout();
                    if !layout.contains(&pane) {
                        continue;
                    }
                    let offered = offers(&arrangement, pane, alone);
                    let read = labels(&offered);
                    for side in SIDES {
                        let beside = Destination::Beside { pane, side };
                        assert_eq!(
                            read.contains(&format!("Split {}", split_name(side))),
                            re_homes(&beside, &pane, alone, layout),
                            "{pane} {alone} split {side:?}"
                        );
                        let edge = read.contains(&format!("  {}", edge_name(side)));
                        assert_eq!(
                            edge,
                            re_homes(&Destination::WindowEdge(side), &pane, alone, layout),
                            "{pane} {alone} edge {side:?}"
                        );
                    }
                }
            }
        }
    }

    /// Every other open window is offered by the title it goes by, and a
    /// session holding none offers no such row at all.
    #[test]
    fn the_session_s_other_windows_are_offered_by_their_titles() {
        let arrangement = Arrangement::new(Layout::Pane('a'));
        let standing = standing(&arrangement, &'a', true);
        assert_eq!(
            labels(&offered(&standing, &windows(["1. b.profile.gz"]))),
            [
                "Move to New Window",
                "Move to Window",
                "  1. b.profile.gz",
                "Close",
            ]
        );
        assert_eq!(
            labels(&offered::<char, Window>(&standing, &[])),
            ["Move to New Window", "Close"]
        );
    }

    /// A move names the window it is to be carried out on, so the windows a
    /// session holds open are told apart by the offers and not by the titles
    /// alone: each row carries the window it was listed from.
    #[test]
    fn a_move_to_a_window_names_that_window() {
        let arrangement = Arrangement::new(Layout::Pane('a'));
        let standing = standing(&arrangement, &'a', true);
        let offered = offered(&standing, &windows(["second", "third", "fourth"]));
        let listed = under(&offered, "Move to Window");
        let named: Vec<&Offer<Window>> = listed.iter().map(|under| &under.offer).collect();
        assert_eq!(
            named,
            [
                &Offer::Window("second"),
                &Offer::Window("third"),
                &Offer::Window("fourth"),
            ]
        );
    }

    /// Each side names the destination that side stands for, so a menu offering
    /// a split and an edge for the same side carries them out apart.
    #[test]
    fn every_offer_carries_out_what_its_row_reads() {
        for side in SIDES {
            assert_eq!(
                deed::<char, Window>(Offer::Split(side), &'a'),
                Deed::Move(Destination::Beside { pane: 'a', side })
            );
            assert_eq!(
                deed::<char, Window>(Offer::Edge(side), &'a'),
                Deed::Move(Destination::WindowEdge(side))
            );
        }
        assert_eq!(
            deed::<char, Window>(Offer::NewWindow, &'a'),
            Deed::Move(Destination::NewWindow)
        );
        assert_eq!(
            deed::<char, Window>(Offer::Window("second"), &'a'),
            Deed::ToWindow("second")
        );
        assert_eq!(
            deed::<char, Window>(Offer::EvenSplit, &'a'),
            Deed::EvenSplit
        );
        assert_eq!(deed::<char, Window>(Offer::Close, &'a'), Deed::Close);
    }

    /// Asking of a window that draws a pane in place of the rest puts them all
    /// back, so the row offering the body and the row offering the panes back
    /// are the one deed.
    #[test]
    fn the_body_and_the_panes_back_are_the_one_deed() {
        assert_eq!(deed::<char, Window>(Offer::Maximize, &'a'), Deed::Maximize);
        assert_eq!(deed::<char, Window>(Offer::Restore, &'a'), Deed::Maximize);
    }
}
