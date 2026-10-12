//! Steps a view has taken, walked back and forth as a browser walks the pages
//! it has shown.

use bviz_graph_layout::Id;
use std::collections::VecDeque;

/// The most steps a history keeps; the oldest give way past it.
const LENGTH: usize = 100;

/// Steps in order, one of them the current.
#[derive(Clone)]
pub(crate) struct History<T> {
    /// Oldest first; never empty, and never longer than [`LENGTH`].
    steps: VecDeque<T>,
    /// Where in `steps` the current step stands.
    at: usize,
}

impl<T: PartialEq> History<T> {
    /// A history holding `current` alone.
    pub(crate) fn new(current: T) -> Self {
        let mut steps = VecDeque::with_capacity(LENGTH);
        steps.push_back(current);
        Self { steps, at: 0 }
    }

    pub(crate) fn current(&self) -> &T {
        &self.steps[self.at]
    }

    /// Makes `next` the current step, after the one that was, dropping the
    /// steps gone back past; a step that is the current already is no step.
    pub(crate) fn push(&mut self, next: T) {
        if *self.current() == next {
            return;
        }
        self.steps.truncate(self.at + 1);
        if self.steps.len() == LENGTH {
            self.steps.pop_front();
        }
        self.steps.push_back(next);
        self.at = self.steps.len() - 1;
    }

    /// Whether there is a step before the current one to step back to.
    pub(crate) fn has_back(&self) -> bool {
        self.at > 0
    }

    /// Rewrites each step by `rewrite`, merging a step rewritten alike to the
    /// one before it into that one.
    pub(crate) fn rewrite(&mut self, mut rewrite: impl FnMut(&T) -> T) {
        let mut steps: VecDeque<T> = VecDeque::with_capacity(LENGTH);
        let mut at = 0;
        for (index, step) in self.steps.iter().enumerate() {
            let step = rewrite(step);
            if steps.back() != Some(&step) {
                steps.push_back(step);
            }
            if index == self.at {
                at = steps.len() - 1;
            }
        }
        self.steps = steps;
        self.at = at;
    }

    /// Steps back to the step before the current one, where there is one.
    pub(crate) fn back(&mut self) -> Option<&T> {
        self.at = self.at.checked_sub(1)?;
        Some(self.current())
    }

    /// Steps on to the step after the current one, where one was gone back
    /// past.
    pub(crate) fn forward(&mut self) -> Option<&T> {
        if self.at + 1 == self.steps.len() {
            return None;
        }
        self.at += 1;
        Some(self.current())
    }
}

/// One place a view has been moved to by name.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Stop<V> {
    Node(Id),
    /// Where the view stood, with no node selected, as the first jump left
    /// it.
    View(V),
}

/// The places a view has been moved to by name, walked back and forth.
pub(crate) struct Jumps<V>(Option<History<Stop<V>>>);

impl<V> Default for Jumps<V> {
    fn default() -> Self {
        Jumps(None)
    }
}

impl<V: Copy + PartialEq> Jumps<V> {
    /// Records a jump onto `id`; the first also records `origin`, where the
    /// view stood as it left.
    pub(crate) fn visit(&mut self, id: Id, origin: Option<Stop<V>>) {
        let onto = Stop::Node(id);
        match (&mut self.0, origin) {
            (Some(history), _) => history.push(onto),
            (None, Some(origin)) => {
                let mut history = History::new(origin);
                history.push(onto);
                self.0 = Some(history);
            }
            (None, None) => self.0 = Some(History::new(onto)),
        }
    }

    pub(crate) fn back(&mut self) -> Option<Stop<V>> {
        self.0.as_mut()?.back().copied()
    }

    pub(crate) fn forward(&mut self) -> Option<Stop<V>> {
        self.0.as_mut()?.forward().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_rewritten_alike_merge_and_the_current_stays_current() {
        let mut history = History::new(0);
        for step in 1..5 {
            history.push(step);
        }
        history.back();
        history.rewrite(|&step| step.min(2));
        assert_eq!(history.current(), &2);
        assert_eq!(history.back(), Some(&1));
        history.forward();
        assert_eq!(history.forward(), None);
    }

    #[test]
    fn back_and_forward_walk_the_steps_pushed() {
        let mut history = History::new(0);
        history.push(1);
        history.push(2);
        assert_eq!(history.back(), Some(&1));
        assert_eq!(history.back(), Some(&0));
        assert_eq!(history.back(), None);
        assert_eq!(history.current(), &0);
        assert_eq!(history.forward(), Some(&1));
        assert_eq!(history.forward(), Some(&2));
        assert_eq!(history.forward(), None);
        assert_eq!(history.current(), &2);
    }

    /// A step pushed after going back drops the steps gone back past, so
    /// forward has nowhere left to go.
    #[test]
    fn a_step_after_going_back_drops_those_ahead() {
        let mut history = History::new(0);
        history.push(1);
        history.push(2);
        history.back();
        history.back();
        history.push(3);
        assert_eq!(history.forward(), None);
        assert_eq!(history.back(), Some(&0));
        assert_eq!(history.forward(), Some(&3));
    }

    #[test]
    fn there_is_a_step_back_only_past_the_first() {
        let mut history = History::new(0);
        assert!(!history.has_back());
        history.push(1);
        assert!(history.has_back());
        history.back();
        assert!(!history.has_back());
    }

    #[test]
    fn pushing_the_current_step_is_no_step() {
        let mut history = History::new(0);
        history.push(1);
        history.push(1);
        assert_eq!(history.back(), Some(&0));
        assert_eq!(history.back(), None);
    }

    #[test]
    fn the_oldest_steps_give_way_past_the_length_kept() {
        let mut history = History::new(0);
        let pushed = LENGTH + 5;
        for step in 1..=pushed {
            history.push(step);
        }
        let mut oldest = *history.current();
        while let Some(&step) = history.back() {
            oldest = step;
        }
        assert_eq!(oldest, pushed + 1 - LENGTH);
        assert_eq!(history.steps.len(), LENGTH);
    }

    /// The node a stop names, where it names one.
    fn node(stop: Option<Stop<()>>) -> Option<Id> {
        match stop? {
            Stop::Node(id) => Some(id),
            Stop::View(()) => None,
        }
    }

    #[test]
    fn the_jumps_walk_back_and_forth_and_a_new_jump_drops_those_ahead() {
        let mut jumps = Jumps::<()>::default();
        assert!(jumps.back().is_none());
        for id in [1, 2, 3] {
            jumps.visit(Id(id), None);
        }
        assert_eq!(node(jumps.back()), Some(Id(2)));
        assert_eq!(node(jumps.back()), Some(Id(1)));
        assert!(jumps.back().is_none());
        assert_eq!(node(jumps.forward()), Some(Id(2)));
        jumps.visit(Id(4), None);
        assert!(jumps.forward().is_none());
        assert_eq!(node(jumps.back()), Some(Id(2)));
    }

    /// The first jump keeps where the view stood as it left, so stepping back
    /// after a single jump returns there; later jumps leave from a stop the
    /// history holds.
    #[test]
    fn the_first_jump_keeps_where_the_view_stood() {
        let mut jumps = Jumps::default();
        jumps.visit(Id(5), Some(Stop::View(())));
        jumps.visit(Id(6), Some(Stop::Node(Id(9))));
        assert_eq!(node(jumps.back()), Some(Id(5)));
        assert_eq!(jumps.back(), Some(Stop::View(())));
        assert!(jumps.back().is_none());
    }
}
