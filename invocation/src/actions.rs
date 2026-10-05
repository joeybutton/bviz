//! Which of a build's actions wait on which, as its action graph (`bazel
//! aquery`) records them.

/// An action of one [`ActionGraph`], by its place among the graph's actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActionIndex(u32);

impl ActionIndex {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A build's actions and the actions each takes an input from. Nothing keeps
/// the dependencies acyclic: a reader walking them must not rely on it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ActionGraph {
    /// By action, the exec path of its primary output, `None` for an action
    /// naming none.
    primary_outputs: Vec<Option<String>>,
    /// By action, where its dependencies start in `dependencies`; one more
    /// entry than there are actions, so that action `i`'s run up to entry
    /// `i + 1`.
    starts: Vec<u32>,
    dependencies: Vec<ActionIndex>,
    /// The same of the actions taking an input from each action.
    dependent_starts: Vec<u32>,
    dependents: Vec<ActionIndex>,
}

impl ActionGraph {
    /// The actions writing `primary_outputs`, in that order, each `(i, j)` of
    /// `edges` saying that the action at position `i` takes an input
    /// from the one at position `j`.
    ///
    /// # Panics
    ///
    /// Where a position in `edges` lies past the last action.
    pub fn new(
        primary_outputs: Vec<Option<String>>,
        mut edges: Vec<(usize, usize)>,
    ) -> ActionGraph {
        let actions = primary_outputs.len();
        assert!(
            u32::try_from(actions).is_ok(),
            "a build holds fewer than u32::MAX actions"
        );
        assert!(
            edges
                .iter()
                .all(|&(dependent, dependency)| dependent.max(dependency) < actions),
            "a dependency names a position past the last of {actions} actions"
        );
        edges.sort_unstable();
        edges.dedup();
        let (dependencies, starts) = adjacency(actions, &edges, |&(from, to)| (from, to));
        let (dependents, dependent_starts) = adjacency(actions, &edges, |&(from, to)| (to, from));
        ActionGraph {
            primary_outputs,
            starts,
            dependencies,
            dependent_starts,
            dependents,
        }
    }

    pub fn len(&self) -> usize {
        self.primary_outputs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.primary_outputs.is_empty()
    }

    /// Every action, in the order of the primary outputs it was built from.
    pub fn actions(&self) -> impl ExactSizeIterator<Item = ActionIndex> + use<> {
        (0..self.len() as u32).map(ActionIndex)
    }

    /// The exec path of `action`'s primary output, `None` for an action naming
    /// none.
    pub fn primary_output(&self, action: ActionIndex) -> Option<&str> {
        self.primary_outputs[action.index()].as_deref()
    }

    /// The actions `action` takes an input from, each once.
    pub fn dependencies(&self, action: ActionIndex) -> &[ActionIndex] {
        let i = action.index();
        &self.dependencies[self.starts[i] as usize..self.starts[i + 1] as usize]
    }

    /// The actions taking an input from `action`, each once.
    pub fn dependents(&self, action: ActionIndex) -> &[ActionIndex] {
        let i = action.index();
        &self.dependents[self.dependent_starts[i] as usize..self.dependent_starts[i + 1] as usize]
    }

    /// The number of dependencies over every action.
    pub fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }
}

/// The `(from, to)` pairs `edge` makes of `edges`, positions under `actions`,
/// as an adjacency list: by action, its `to`s in the order of `edges`, and
/// where each action's start.
fn adjacency(
    actions: usize,
    edges: &[(usize, usize)],
    edge: impl Fn(&(usize, usize)) -> (usize, usize),
) -> (Vec<ActionIndex>, Vec<u32>) {
    let mut starts = vec![0u32; actions + 1];
    for (from, _) in edges.iter().map(&edge) {
        starts[from + 1] += 1;
    }
    for i in 1..starts.len() {
        starts[i] += starts[i - 1];
    }
    let mut filled = starts.clone();
    let mut adjacent = vec![ActionIndex(0); edges.len()];
    for (from, to) in edges.iter().map(&edge) {
        let slot = &mut filled[from];
        adjacent[*slot as usize] = ActionIndex(to as u32);
        *slot += 1;
    }
    (adjacent, starts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_depends_on_each_of_its_dependencies_once() {
        let outputs = [Some("bazel-out/a.o"), Some("bazel-out/a.h"), None];
        let graph = ActionGraph::new(
            outputs.map(|output| output.map(str::to_owned)).to_vec(),
            vec![(2, 0), (0, 1), (2, 0), (2, 1)],
        );
        let [compile, generate, link] = graph.actions().collect::<Vec<_>>()[..] else {
            panic!("three actions");
        };

        assert_eq!(graph.len(), 3);
        assert_eq!(graph.dependency_count(), 3);
        assert_eq!(graph.dependencies(compile), [generate]);
        assert_eq!(graph.dependencies(generate), []);
        assert_eq!(graph.dependencies(link), [compile, generate]);
        assert_eq!(graph.dependents(generate), [compile, link]);
        assert_eq!(graph.dependents(link), []);
        assert_eq!(graph.primary_output(compile), Some("bazel-out/a.o"));
        assert_eq!(graph.primary_output(link), None);
    }

    #[test]
    #[should_panic(expected = "past the last of 1 actions")]
    fn a_dependency_past_the_last_action_is_refused() {
        ActionGraph::new(vec![None], vec![(0, 1)]);
    }
}
