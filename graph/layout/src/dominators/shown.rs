//! A dominator tree as drawn where a filter shows only some of its points.

use std::cmp::Reverse;
use std::num::NonZeroU32;

use super::Dominators;
use crate::intern::Id;

/// What a node of a [`ShownTree`] is drawn under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Above {
    /// The top of the build, as the roots are, and as a root's shown points
    /// are where the root is hidden.
    Roots,
    /// What the roots share, as the shared tops are, and as a shared top's
    /// shown points are where the top is hidden.
    Shared,
    Node(u32),
}

/// Where a shown node of a [`ShownTree`] is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placing {
    pub above: Above,
    /// Where the node stands among the siblings drawn under `above`.
    pub place: u32,
    /// How many hidden dominators stand between the node and `above`.
    pub spliced: u32,
}

/// A [`Dominators`] tree with its hidden points spliced out, numbered as the
/// tree numbers them. Each shown point is drawn under its nearest shown
/// dominator, which dominates it as surely as the hidden ones between, and
/// weighs the shown points it dominates. A hidden point is not drawn, and
/// with it goes every hidden point it dominates that dominates no shown one.
///
/// Siblings come heaviest first by shown weight, so a row gathering the
/// lightest of them gathers a tail, and among equal weights in the order the
/// tree numbers its nodes: breadth first from its tops, which every run of the
/// queries gives alike, so that siblings under one dominator in the tree keep
/// its order and those spliced up from deeper follow those from shallower.
#[derive(PartialEq, Eq)]
pub struct ShownTree {
    /// By node, how many shown points it dominates, itself among them where it
    /// is shown.
    weight: Vec<u32>,
    /// By node, where it is drawn; none for a hidden node.
    placing: Vec<Option<Slotted>>,
    /// By slot, where the nodes drawn under it start in `drawn`, and past the
    /// last slot the length of `drawn`.
    start: Vec<u32>,
    drawn: Vec<u32>,
    shared_weight: u32,
}

/// A [`Placing`] whose `above` is a slot.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Slotted {
    slot: Slot,
    place: u32,
    spliced: u32,
}

/// What nodes are drawn under: a node, or past the nodes
/// [`ShownTree::roots_slot`] and the slot after it for the shared tops. Held
/// one past its index, so that a node not drawn costs no more than one drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Slot(NonZeroU32);

impl Slot {
    fn new(index: u32) -> Self {
        Self(NonZeroU32::MIN.saturating_add(index))
    }

    fn index(self) -> u32 {
        self.0.get() - 1
    }
}

impl ShownTree {
    /// `tree` with the points `shows` refuses spliced out.
    pub fn of(tree: &Dominators, shows: impl Fn(Id) -> bool) -> Self {
        let n = tree.total();
        let shown: Vec<bool> = (0..n).map(|node| shows(tree.id(node))).collect();
        // A node's children are numbered after it.
        let mut weight = vec![0u32; n as usize];
        for node in (0..n).rev() {
            let below: u32 = tree.children(node).map(|c| weight[c as usize]).sum();
            weight[node as usize] = below + u32::from(shown[node as usize]);
        }
        let (roots_slot, shared_slot) = (n, n + 1);
        // By node, the slot it hangs from and the hidden nodes in between.
        let mut hangs: Vec<(u32, u32)> = vec![(roots_slot, 0); n as usize];
        for top in tree.shared_tops() {
            hangs[top as usize] = (shared_slot, 0);
        }
        for node in 0..n {
            let (slot, spliced) = hangs[node as usize];
            let under = match shown[node as usize] {
                true => (node, 0),
                false => (slot, spliced + 1),
            };
            for child in tree.children(node) {
                hangs[child as usize] = under;
            }
        }
        let slots = n as usize + 2;
        let mut start = vec![0u32; slots + 1];
        for node in (0..n).filter(|&node| shown[node as usize]) {
            start[hangs[node as usize].0 as usize + 1] += 1;
        }
        for slot in 1..start.len() {
            start[slot] += start[slot - 1];
        }
        let mut filled = start.clone();
        let mut drawn = vec![0u32; start[slots] as usize];
        for node in (0..n).filter(|&node| shown[node as usize]) {
            let at = &mut filled[hangs[node as usize].0 as usize];
            drawn[*at as usize] = node;
            *at += 1;
        }
        let mut placing: Vec<Option<Slotted>> = vec![None; n as usize];
        for slot in 0..slots {
            let siblings = &mut drawn[start[slot] as usize..start[slot + 1] as usize];
            siblings.sort_by_key(|&node| Reverse(weight[node as usize]));
            for (place, &node) in siblings.iter().enumerate() {
                placing[node as usize] = Some(Slotted {
                    slot: Slot::new(slot as u32),
                    place: place as u32,
                    spliced: hangs[node as usize].1,
                });
            }
        }
        let shared_weight = tree.shared_tops().map(|top| weight[top as usize]).sum();
        Self {
            weight,
            placing,
            start,
            drawn,
            shared_weight,
        }
    }

    /// `tree` with every point shown, drawn as it stands.
    pub fn whole(tree: &Dominators) -> Self {
        Self::of(tree, |_| true)
    }

    fn roots_slot(&self) -> u32 {
        self.weight.len() as u32
    }

    fn under(&self, slot: u32) -> &[u32] {
        let slot = slot as usize;
        &self.drawn[self.start[slot] as usize..self.start[slot + 1] as usize]
    }

    /// How many points are shown, which the weights of the nodes drawn under
    /// [`Above::Roots`] and [`Above::Shared`] sum to.
    pub fn total(&self) -> u32 {
        self.drawn.len() as u32
    }

    /// The nodes drawn under the top of the build.
    pub fn roots(&self) -> &[u32] {
        self.under(self.roots_slot())
    }

    /// The nodes drawn under what the roots share.
    pub fn shared_tops(&self) -> &[u32] {
        self.under(self.roots_slot() + 1)
    }

    /// How many shown points the [shared tops](Self::shared_tops) dominate
    /// between them.
    pub fn shared_weight(&self) -> u32 {
        self.shared_weight
    }

    /// The nodes drawn under `node`.
    pub fn children(&self, node: u32) -> &[u32] {
        self.under(node)
    }

    /// How many shown points `node` dominates, itself among them where it is
    /// shown.
    pub fn weight(&self, node: u32) -> u32 {
        self.weight[node as usize]
    }

    /// Where `node` is drawn; absent where it is hidden.
    pub fn placing(&self, node: u32) -> Option<Placing> {
        let Slotted {
            slot,
            place,
            spliced,
        } = self.placing[node as usize]?;
        let roots = self.roots_slot();
        let above = match slot.index() {
            slot if slot == roots => Above::Roots,
            slot if slot == roots + 1 => Above::Shared,
            node => Above::Node(node),
        };
        Some(Placing {
            above,
            place,
            spliced,
        })
    }

    pub fn shows(&self, node: u32) -> bool {
        self.placing[node as usize].is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{configured, node, random_build, random_indices, tree_from, tree_of};
    use super::*;
    use crate::intern::Names;
    use crate::metrics::Unit;

    /// The drawn tree from `top` down, each node as its name and shown
    /// weight, its children braced after it.
    fn drawn(shown: &ShownTree, tree: &Dominators, names: &Names, top: &[u32]) -> String {
        let mut out = Vec::new();
        for &node in top {
            out.push(format!(
                "{} {}",
                names.resolve(tree.id(node)),
                shown.weight(node)
            ));
            let children = shown.children(node);
            if !children.is_empty() {
                out.push(format!("{{ {} }}", drawn(shown, tree, names, children)));
            }
        }
        out.join(" ")
    }

    /// The shown tree of `tree` hiding the points `hidden` names.
    fn hiding(tree: &Dominators, names: &Names, hidden: &[&str]) -> ShownTree {
        let hidden: Vec<Id> = hidden.iter().map(|name| names.get(name).unwrap()).collect();
        ShownTree::of(tree, |id| !hidden.contains(&id))
    }

    #[test]
    fn a_hidden_dominator_s_shown_points_are_drawn_under_its_nearest_shown_one() {
        let (graph, names) = configured(&[("lib", "mid"), ("mid", "bin"), ("bin", "test")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let shown = hiding(&tree, &names, &["mid", "bin"]);
        assert_eq!(
            drawn(&shown, &tree, &names, shown.roots()),
            "test 2 { lib 1 }"
        );
        let lib = node(&tree, &names, "lib");
        let test = node(&tree, &names, "test");
        let placing = shown.placing(lib).unwrap();
        assert_eq!((placing.above, placing.spliced), (Above::Node(test), 2));
        assert!(!shown.shows(node(&tree, &names, "mid")));
        assert_eq!(shown.placing(node(&tree, &names, "mid")), None);
        assert_eq!(shown.total(), 2);
    }

    /// Siblings spliced together are weighed by what they show, and the
    /// heaviest so comes first.
    #[test]
    fn siblings_are_ordered_by_the_points_they_show() {
        let (graph, names) = configured(&[
            ("a1", "a"),
            ("a2", "a"),
            ("a3", "a"),
            ("a", "app"),
            ("b1", "b"),
            ("b", "app"),
        ]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let whole = ShownTree::whole(&tree);
        assert!(drawn(&whole, &tree, &names, whole.roots()).starts_with("app 7 { a 4"));
        let shown = hiding(&tree, &names, &["a1", "a2", "a3"]);
        assert_eq!(
            drawn(&shown, &tree, &names, shown.roots()),
            "app 4 { b 2 { b1 1 } a 1 }"
        );
    }

    #[test]
    fn a_hidden_subtree_showing_nothing_vanishes() {
        let (graph, names) = configured(&[("x1", "x"), ("x", "app"), ("y", "app")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let shown = hiding(&tree, &names, &["x", "x1"]);
        assert_eq!(drawn(&shown, &tree, &names, shown.roots()), "app 2 { y 1 }");
        assert_eq!(shown.weight(node(&tree, &names, "x")), 0);
    }

    /// A hidden root's shown points stand under the top of the build beside
    /// the roots, heaviest first, and a hidden shared top's beside the
    /// shared tops; the tree's own standings are untouched.
    #[test]
    fn a_hidden_top_s_shown_points_take_its_place_in_its_row() {
        let (graph, names) = configured(&[
            ("lib", "a"),
            ("lib2", "lib"),
            ("lib3", "lib2"),
            ("base", "a"),
            ("base", "b"),
            ("under", "base"),
            ("under2", "under"),
        ]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let whole = ShownTree::whole(&tree);
        assert_eq!(
            drawn(&whole, &tree, &names, whole.shared_tops()),
            "base 3 { under 2 { under2 1 } }"
        );
        let standing = tree.standing(node(&tree, &names, "a"));
        let shown = hiding(&tree, &names, &["a", "base"]);
        assert_eq!(
            drawn(&shown, &tree, &names, shown.roots()),
            "lib 3 { lib2 2 { lib3 1 } } b 1"
        );
        let lib = shown.placing(node(&tree, &names, "lib")).unwrap();
        assert_eq!((lib.above, lib.place, lib.spliced), (Above::Roots, 0, 1));
        assert_eq!(
            drawn(&shown, &tree, &names, shown.shared_tops()),
            "under 2 { under2 1 }"
        );
        let under = shown.placing(node(&tree, &names, "under")).unwrap();
        assert_eq!((under.above, under.spliced), (Above::Shared, 1));
        assert_eq!((shown.shared_weight(), shown.total()), (2, 6));
        assert_eq!(tree.standing(node(&tree, &names, "a")), standing);
        let shown = hiding(&tree, &names, &["base", "under", "under2"]);
        assert!(shown.shared_tops().is_empty());
        assert_eq!(shown.shared_weight(), 0);
    }

    #[test]
    fn a_tree_showing_nothing_draws_nothing() {
        let (graph, names) = configured(&[("lib", "bin")]);
        let tree = tree_from(&graph, &names, &["bin"]);
        let shown = ShownTree::of(&tree, |_| false);
        assert_eq!(
            (shown.total(), shown.roots(), shown.shared_weight()),
            (0, &[][..], 0)
        );
    }

    /// How many source files a random build of actions reads from.
    const SOURCES: usize = 5;

    /// On random builds of configured targets and of actions, whose files
    /// are points of no tree, a tree showing every point is drawn as the tree
    /// stands, and one showing some draws each shown point under its nearest
    /// shown dominator, weighed by the shown points below it, siblings
    /// heaviest first and then in node order.
    #[test]
    fn random_trees_are_drawn_as_their_shown_points_stand() {
        let mut next = random_indices();
        let builds = [(30, 45), (120, 160), (200, 500)];
        let units = [(Unit::ConfiguredTarget, 0), (Unit::Action, SOURCES)];
        for ((nodes, edges), (unit, sources)) in
            builds.into_iter().flat_map(|b| units.map(|u| (b, u)))
        {
            let deps: Vec<(usize, usize)> = (0..edges)
                .map(|_| {
                    let (a, b) = (next(nodes), next(nodes));
                    (a.max(b), a.min(b))
                })
                .collect();
            let (graph, names) = random_build(unit, nodes, &deps, sources, &mut next);
            let tree = tree_of(&graph, &names, unit);
            let whole = ShownTree::whole(&tree);
            assert_eq!(whole.roots(), Vec::from_iter(tree.roots()));
            assert_eq!(whole.shared_tops(), Vec::from_iter(tree.shared_tops()));
            assert_eq!(whole.shared_weight(), tree.shared_weight());
            for v in 0..tree.total() {
                assert_eq!(whole.children(v), Vec::from_iter(tree.children(v)));
                assert_eq!(whole.weight(v), tree.weight(v));
                assert_eq!(whole.placing(v).unwrap().spliced, 0);
            }
            let hidden: Vec<bool> = (0..names.len()).map(|_| next(3) == 0).collect();
            let shown = ShownTree::of(&tree, |id| !hidden[id.0 as usize]);
            let shows = |v: u32| !hidden[tree.id(v).0 as usize];
            let mut count = 0;
            for v in 0..tree.total() {
                let mut subtree = vec![v];
                let mut at = 0;
                while let Some(&w) = subtree.get(at) {
                    subtree.extend(tree.children(w));
                    at += 1;
                }
                let weight = subtree.iter().filter(|&&w| shows(w)).count() as u32;
                assert_eq!(shown.weight(v), weight);
                assert_eq!(shown.shows(v), shows(v));
                if !shows(v) {
                    continue;
                }
                count += 1;
                let up = std::iter::successors(tree.parent(v), |&p| tree.parent(p));
                let mut spliced = 0;
                let mut nearest = None;
                for p in up {
                    if shows(p) {
                        nearest = Some(p);
                        break;
                    }
                    spliced += 1;
                }
                let top = std::iter::successors(Some(v), |&p| tree.parent(p))
                    .last()
                    .unwrap();
                let above = match nearest {
                    Some(p) => Above::Node(p),
                    None if tree.shared_tops().contains(&top) => Above::Shared,
                    None => Above::Roots,
                };
                let placing = shown.placing(v).unwrap();
                assert_eq!((placing.above, placing.spliced), (above, spliced));
                let siblings = match above {
                    Above::Roots => shown.roots(),
                    Above::Shared => shown.shared_tops(),
                    Above::Node(p) => shown.children(p),
                };
                assert_eq!(siblings[placing.place as usize], v);
            }
            assert_eq!(shown.total(), count);
            let weighed = |row: &[u32]| row.iter().map(|&v| shown.weight(v)).sum::<u32>();
            assert_eq!(shown.shared_weight(), weighed(shown.shared_tops()));
            assert_eq!(
                shown.total(),
                weighed(shown.roots()) + shown.shared_weight()
            );
            let rows = (0..tree.total()).map(|v| shown.children(v));
            for row in rows.chain([shown.roots(), shown.shared_tops()]) {
                let order: Vec<(Reverse<u32>, u32)> =
                    row.iter().map(|&c| (Reverse(shown.weight(c)), c)).collect();
                assert!(order.is_sorted(), "{order:?}");
            }
        }
    }
}
