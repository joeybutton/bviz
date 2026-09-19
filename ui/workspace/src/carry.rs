//! The drags the window system carries for a window — the one that window
//! handed over, and the one it carries over the window — and what each report
//! of them leaves the window to do.

use gpui::{Pixels, PlatformDragEnd, Point};

/// The drag a window handed to the window system, while it is out there.
struct Handed<P> {
    panel: P,
    /// Whether a drop has already put the panel where the drag was aimed, so
    /// that a release the window system goes on to report as nothing having
    /// taken the panel does not put it somewhere else as well.
    landed: bool,
}

/// A drag the window system is carrying over a window.
pub(crate) struct Incoming<P, D> {
    /// The panel it holds, read from the window that handed it over as the drag
    /// arrived, so that what a drop of it moves is settled before anything is
    /// aimed at.
    pub(crate) panel: P,
    /// Where the window system last reported the drag over the window.
    pub(crate) at: Point<Pixels>,
    /// Where the last frame drawn marked the panel landing, which is where a
    /// drop of the drag puts it, so that what the drag shows is what it does.
    /// `None` until a frame has marked anything at all, and where the frame
    /// that did marked nothing.
    pub(crate) to: Option<D>,
}

/// What the window system's drags leave a window holding. The two are the one
/// drag while it is carried back over the window it started in.
pub(crate) struct Carrying<P, D> {
    handed: Option<Handed<P>>,
    incoming: Option<Incoming<P, D>>,
}

/// What the window system reports of a drag, once the window has read into it
/// what only the window knows.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Reported<P, D> {
    /// A drag holding `panel` came over the window, at that position relative
    /// to it. `None` where no window of the session owns up to a drag out
    /// there, which holds nothing this one could move.
    Entered { panel: Option<P>, at: Point<Pixels> },
    /// The drag over the window moved to that position relative to it.
    Moved(Point<Pixels>),
    /// The drag over the window left it without being dropped there.
    Left,
    /// The drag over the window was dropped, `aimed` giving where the drop's
    /// own position puts the panel, for a drop no frame has marked a landing
    /// for.
    Dropped { aimed: Option<D> },
    /// The drag this window handed over is over.
    Ended(PlatformDragEnd),
}

/// What a report leaves the window to carry out.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Taken<P, D> {
    /// Nothing beyond what the report left the drags themselves.
    Nothing,
    /// Puts `panel` at `to`, where the drag over the window was dropped.
    Land { panel: P, to: D },
    /// Ends the drag this window handed over. `detach` says the user let go of
    /// it where nothing took the panel, which asks for a window of its own.
    Ended { panel: P, detach: bool },
}

impl<P: Copy + PartialEq, D> Carrying<P, D> {
    pub(crate) fn new() -> Self {
        Self {
            handed: None,
            incoming: None,
        }
    }

    /// The panel the drag this window handed over is carrying, while it is out
    /// there.
    pub(crate) fn handed_panel(&self) -> Option<P> {
        self.handed.as_ref().map(|handed| handed.panel)
    }

    /// Takes up the drag of `panel` the window system has just taken over.
    pub(crate) fn hand_over(&mut self, panel: P) {
        self.handed = Some(Handed {
            panel,
            landed: false,
        });
    }

    /// Whether a press on a panel's tab starts a gesture, which it does not
    /// while a drag this window handed over is still out there: that one is the
    /// window system's to end, and a second gesture here would leave it with
    /// nothing to report to.
    pub(crate) fn takes_a_press(&self) -> bool {
        self.handed.is_none()
    }

    /// The drag the window system is carrying over the window, while one is.
    pub(crate) fn incoming(&self) -> Option<&Incoming<P, D>> {
        self.incoming.as_ref()
    }

    /// The panel a drag over the window holds where no frame has marked where
    /// that drag would land, which is what a drop of it has only its own
    /// position to go on.
    pub(crate) fn unaimed(&self) -> Option<P> {
        let incoming = self.incoming.as_ref()?;
        incoming.to.is_none().then_some(incoming.panel)
    }

    /// Marks where the frame being drawn puts a drop of the drag over the
    /// window.
    pub(crate) fn aim(&mut self, to: Option<D>) {
        if let Some(incoming) = self.incoming.as_mut() {
            incoming.to = to;
        }
    }

    /// Takes `report` up, and says what it leaves the window to carry out.
    pub(crate) fn reports(&mut self, report: Reported<P, D>) -> Taken<P, D> {
        match report {
            Reported::Entered { panel, at } => {
                self.incoming = panel.map(|panel| Incoming {
                    panel,
                    at,
                    to: None,
                });
                Taken::Nothing
            }
            Reported::Moved(at) => {
                if let Some(incoming) = self.incoming.as_mut() {
                    incoming.at = at;
                }
                Taken::Nothing
            }
            Reported::Left => {
                self.incoming = None;
                Taken::Nothing
            }
            Reported::Dropped { aimed } => {
                let Some(incoming) = self.incoming.take() else {
                    return Taken::Nothing;
                };
                let Some(to) = incoming.to.or(aimed) else {
                    return Taken::Nothing;
                };
                if let Some(handed) = self.handed.as_mut()
                    && handed.panel == incoming.panel
                {
                    handed.landed = true;
                }
                Taken::Land {
                    panel: incoming.panel,
                    to,
                }
            }
            Reported::Ended(end) => {
                let Some(handed) = self.handed.take() else {
                    return Taken::Nothing;
                };
                // The drag that ended is the one carried over the window, there
                // being no second one for the window system to carry.
                self.incoming = None;
                Taken::Ended {
                    panel: handed.panel,
                    detach: end == PlatformDragEnd::Released && !handed.landed,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px};

    /// The panels two windows of a session hold, named apart so that what a
    /// report moves reads as the panel it names.
    const HERE: &str = "here";
    const ELSEWHERE: &str = "elsewhere";

    /// Where a drag is reported, which the transitions read only as one
    /// position or another.
    fn at(x: f32) -> Point<Pixels> {
        point(px(x), px(100.))
    }

    fn carrying() -> Carrying<&'static str, &'static str> {
        Carrying::new()
    }

    /// A window the drag was never carried over hears it end all the same,
    /// having handed it over: the panel goes where the end asks for it.
    #[test]
    fn a_drag_that_never_came_back_over_the_window_still_ends_there() {
        let mut carrying = carrying();
        carrying.hand_over(HERE);
        assert_eq!(
            carrying.reports(Reported::Ended(PlatformDragEnd::Released)),
            Taken::Ended {
                panel: HERE,
                detach: true
            }
        );
    }

    /// An end reported to a window holding no drag of its own moves nothing:
    /// only the window that handed a drag over hears how it finished.
    #[test]
    fn an_end_with_no_drag_handed_over_moves_nothing() {
        let mut carrying = carrying();
        for end in [
            PlatformDragEnd::Taken,
            PlatformDragEnd::Released,
            PlatformDragEnd::Cancelled,
        ] {
            assert_eq!(carrying.reports(Reported::Ended(end)), Taken::Nothing);
        }
    }

    /// The end of the drag a window handed over retires the drag carried over
    /// that window too: there is one drag out there, and it is over.
    #[test]
    fn an_end_retires_the_drag_carried_over_the_window() {
        let mut carrying = carrying();
        carrying.hand_over(HERE);
        carrying.reports(Reported::Entered {
            panel: Some(HERE),
            at: at(10.0),
        });
        carrying.reports(Reported::Ended(PlatformDragEnd::Cancelled));
        assert!(carrying.incoming().is_none());
    }

    /// A drop no frame has marked a landing for, and whose own position lands
    /// nowhere either, moves nothing.
    #[test]
    fn a_drop_aimed_at_nothing_moves_nothing() {
        let mut carrying = carrying();
        carrying.reports(Reported::Entered {
            panel: Some(ELSEWHERE),
            at: at(10.0),
        });
        assert_eq!(
            carrying.reports(Reported::Dropped { aimed: None }),
            Taken::Nothing
        );
        assert!(carrying.incoming().is_none());
    }

    /// A drag dropped before any frame has marked where it lands is put where
    /// its own position puts it, so that a flick in and a release still moves
    /// the panel.
    #[test]
    fn a_drop_with_no_frame_between_lands_where_its_own_position_says() {
        let mut carrying = carrying();
        carrying.reports(Reported::Entered {
            panel: Some(ELSEWHERE),
            at: at(10.0),
        });
        assert_eq!(carrying.unaimed(), Some(ELSEWHERE));
        assert_eq!(
            carrying.reports(Reported::Dropped {
                aimed: Some("under the cursor")
            }),
            Taken::Land {
                panel: ELSEWHERE,
                to: "under the cursor"
            }
        );
    }

    /// Where a frame has marked a landing, that is where the drop lands, and
    /// the drop's own position is not asked for at all.
    #[test]
    fn a_drop_lands_where_the_last_frame_marked_it() {
        let mut carrying = carrying();
        carrying.reports(Reported::Entered {
            panel: Some(ELSEWHERE),
            at: at(10.0),
        });
        carrying.aim(Some("marked"));
        assert_eq!(carrying.unaimed(), None);
        assert_eq!(
            carrying.reports(Reported::Dropped { aimed: None }),
            Taken::Land {
                panel: ELSEWHERE,
                to: "marked"
            }
        );
    }

    /// A drag carried over a window no window of the session owns up to holds
    /// nothing that window could move, and a drop of it does nothing.
    #[test]
    fn a_drag_naming_no_panel_is_not_taken_up() {
        let mut carrying = carrying();
        carrying.reports(Reported::Entered {
            panel: None,
            at: at(10.0),
        });
        assert!(carrying.incoming().is_none());
        assert_eq!(
            carrying.reports(Reported::Dropped {
                aimed: Some("under the cursor")
            }),
            Taken::Nothing
        );
    }

    /// A drag carried back into the window it started in lands its panel there
    /// exactly as one carried in from elsewhere does.
    #[test]
    fn a_drop_into_the_window_the_drag_started_in_lands_there() {
        let mut carrying = carrying();
        carrying.hand_over(HERE);
        carrying.reports(Reported::Entered {
            panel: Some(HERE),
            at: at(10.0),
        });
        carrying.aim(Some("marked"));
        assert_eq!(
            carrying.reports(Reported::Dropped { aimed: None }),
            Taken::Land {
                panel: HERE,
                to: "marked"
            }
        );
    }

    /// A release the window system reports as nothing having taken the panel
    /// asks for a window of its own only where the panel is still where it
    /// started: a drop that has already moved it is not undone by one.
    #[test]
    fn a_release_after_the_panel_landed_asks_for_no_window_of_its_own() {
        let mut carrying = carrying();
        carrying.hand_over(HERE);
        carrying.reports(Reported::Entered {
            panel: Some(HERE),
            at: at(10.0),
        });
        carrying.aim(Some("marked"));
        carrying.reports(Reported::Dropped { aimed: None });
        assert_eq!(
            carrying.reports(Reported::Ended(PlatformDragEnd::Released)),
            Taken::Ended {
                panel: HERE,
                detach: false
            }
        );
    }

    /// A drag another window took the panel from has already moved it, and
    /// one called off leaves it where it stands.
    #[test]
    fn only_a_release_nothing_took_asks_for_a_window_of_its_own() {
        for (end, detach) in [
            (PlatformDragEnd::Released, true),
            (PlatformDragEnd::Taken, false),
            (PlatformDragEnd::Cancelled, false),
        ] {
            let mut carrying = carrying();
            carrying.hand_over(HERE);
            assert_eq!(
                carrying.reports(Reported::Ended(end)),
                Taken::Ended {
                    panel: HERE,
                    detach
                }
            );
        }
    }

    /// A drag carried out of the window is let go of, and a drop that follows
    /// it moves nothing.
    #[test]
    fn a_drag_carried_back_out_is_let_go_of() {
        let mut carrying = carrying();
        carrying.reports(Reported::Entered {
            panel: Some(ELSEWHERE),
            at: at(10.0),
        });
        carrying.aim(Some("marked"));
        carrying.reports(Reported::Left);
        assert!(carrying.incoming().is_none());
        assert_eq!(
            carrying.reports(Reported::Dropped { aimed: None }),
            Taken::Nothing
        );
    }

    /// A move carries the drag to where it reports, which is where a drop with
    /// no frame behind it is read from.
    #[test]
    fn a_move_carries_the_drag_to_where_it_reports() {
        let mut carrying = carrying();
        carrying.reports(Reported::Entered {
            panel: Some(ELSEWHERE),
            at: at(10.0),
        });
        carrying.reports(Reported::Moved(at(40.0)));
        assert_eq!(
            carrying.incoming().map(|incoming| incoming.at),
            Some(at(40.0))
        );
    }

    /// A press on another panel's tab while a drag is out there starts no
    /// second gesture, which would leave the first with nothing to report to.
    #[test]
    fn no_press_is_taken_up_while_a_drag_is_handed_over() {
        let mut carrying = carrying();
        assert!(carrying.takes_a_press());
        carrying.hand_over(HERE);
        assert!(!carrying.takes_a_press());
        carrying.reports(Reported::Ended(PlatformDragEnd::Cancelled));
        assert!(carrying.takes_a_press());
    }
}
