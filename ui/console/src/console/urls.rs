//! The URLs a console's viewport draws, listed again only once it shows
//! something else.

use bviz_terminal::Terminal;
use gpui::SharedString;
use std::rc::Rc;

/// A URL the viewport draws.
pub(super) struct DrawnUrl {
    /// The URL in full, which names the element laid over each of its rows as
    /// well as reading in that element's hint.
    pub(super) text: SharedString,
    /// The first and last column it covers on each viewport row, from the top.
    pub(super) spans: Vec<Option<(usize, usize)>>,
}

/// What a viewport shows: the frame its terminal was built for, the size it
/// is wrapped to, and how far it is scrolled.
#[derive(Clone, Copy, PartialEq)]
struct Shown {
    frame: usize,
    columns: usize,
    rows: usize,
    offset: usize,
}

/// The URLs the viewport drew when last listed, and what it showed then.
#[derive(Default)]
pub(super) struct DrawnUrls(Option<(Shown, Rc<[DrawnUrl]>)>);

impl DrawnUrls {
    /// The URLs `terminal`, built for `frame`, draws, listed afresh only where
    /// it shows something else than when they were last listed.
    pub(super) fn of(&mut self, terminal: &mut Terminal, frame: usize) -> Rc<[DrawnUrl]> {
        let shown = Shown {
            frame,
            columns: terminal.columns(),
            rows: terminal.rows(),
            offset: terminal.display_offset(),
        };
        if let Some((listed, urls)) = &self.0
            && *listed == shown
        {
            return Rc::clone(urls);
        }
        let urls: Rc<[DrawnUrl]> = (terminal.urls().iter())
            .map(|url| DrawnUrl {
                text: url.text().to_owned().into(),
                spans: url.spans().to_vec(),
            })
            .collect();
        self.0 = Some((shown, Rc::clone(&urls)));
        urls
    }
}
