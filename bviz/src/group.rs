//! Which of the files named on the command line open as one document: the
//! outputs of one Bazel invocation, told by the invocation id each carries.

use bviz_invocation::InvocationId;
use std::iter;

/// The kinds of file that carry an invocation id. An invocation's document
/// holds one of each.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Profile,
    Bep,
}

/// A profile or a build event stream.
pub(crate) struct File<T> {
    pub(crate) file: T,
    pub(crate) kind: Kind,
    /// `None` where the file carries no id.
    pub(crate) invocation: Option<InvocationId>,
}

/// What `bviz_cli::Sources` names, each file with its kind and the invocation
/// it carries the id of.
pub(crate) enum Given<T> {
    Graph { graph: T, files: Vec<File<T>> },
    Files { first: File<T>, rest: Vec<File<T>> },
}

/// What a window opens as one document.
pub(crate) enum Opening<T> {
    Alone(T),
    /// Files of the invocation `id`, in the order they were given.
    Invocation {
        id: InvocationId,
        first: T,
        second: T,
        rest: Vec<T>,
    },
}

/// A document while the files after its first are still being gone through.
enum Gathering<T> {
    Alone(T),
    Invocation(Gathered<T>),
}

/// The files gathered for the invocation `id`.
struct Gathered<T> {
    id: InvocationId,
    first: T,
    rest: Vec<T>,
    /// The kinds among those files.
    held: Vec<Kind>,
}

impl<T> Gathering<T> {
    /// What `file` opens as where no invocation gathered so far is its own.
    fn begin(file: File<T>) -> Self {
        match file.invocation {
            Some(id) => Self::Invocation(Gathered {
                id,
                first: file.file,
                rest: Vec::new(),
                held: vec![file.kind],
            }),
            None => Self::Alone(file.file),
        }
    }

    fn invocation(&mut self, id: &InvocationId) -> Option<&mut Gathered<T>> {
        match self {
            Self::Invocation(gathered) if gathered.id == *id => Some(gathered),
            _ => None,
        }
    }

    /// An invocation's document holds two files at least; an invocation only
    /// one file was gathered for opens as that file.
    fn gathered(self) -> Opening<T> {
        match self {
            Self::Alone(file) => Opening::Alone(file),
            Self::Invocation(gathered) => {
                let mut rest = gathered.rest.into_iter();
                match rest.next() {
                    Some(second) => Opening::Invocation {
                        id: gathered.id,
                        first: gathered.first,
                        second,
                        rest: rest.collect(),
                    },
                    None => Opening::Alone(gathered.first),
                }
            }
        }
    }
}

/// The invocation `files` name, where those carrying an id all carry the same
/// one.
fn only_invocation<T>(files: &[File<T>]) -> Option<&InvocationId> {
    let mut named = files.iter().filter_map(|file| file.invocation.as_ref());
    let first = named.next()?;
    named.all(|id| id == first).then_some(first)
}

/// What `given` names as the documents a window opens, each standing where its
/// first file was given.
///
/// Files carrying one invocation id form one document, which takes one file of
/// each [`Kind`]; a further file of a kind the document holds opens alone, as
/// does a file carrying no id. A graph carries none either: nothing tells which
/// build it describes, so it is taken for the one invocation the files with it
/// name, and opens alone where they name several or none.
pub(crate) fn by_invocation<T>(given: Given<T>) -> (Opening<T>, Vec<Opening<T>>) {
    let (mut leading, files) = match given {
        Given::Graph { graph, files } => {
            let leading = match only_invocation(&files) {
                Some(id) => Gathering::Invocation(Gathered {
                    id: id.clone(),
                    first: graph,
                    rest: Vec::new(),
                    held: Vec::new(),
                }),
                None => Gathering::Alone(graph),
            };
            (leading, files)
        }
        Given::Files { first, rest } => (Gathering::begin(first), rest),
    };
    let mut others = Vec::with_capacity(files.len());
    for file in files {
        let gathered = file.invocation.as_ref().and_then(|id| {
            iter::once(&mut leading)
                .chain(&mut others)
                .find_map(|gathering| gathering.invocation(id))
        });
        let apart = match gathered {
            Some(gathered) if !gathered.held.contains(&file.kind) => {
                gathered.held.push(file.kind);
                gathered.rest.push(file.file);
                continue;
            }
            Some(_) => Gathering::Alone(file.file),
            None => Gathering::begin(file),
        };
        others.push(apart);
    }
    (
        leading.gathered(),
        others.into_iter().map(Gathering::gathered).collect(),
    )
}

/// Characters of a UUID's first group, which is as much of an invocation id as
/// a label shows.
const SHORT_ID_CHARS: usize = 8;

/// How the invocation `id` reads as its document's label: by the first group
/// of the UUID Bazel names an invocation with, and by as many leading
/// characters of an id that is not one.
pub(crate) fn label(id: &InvocationId) -> String {
    let id = id.as_str();
    let group = id.split('-').find(|group| !group.is_empty()).unwrap_or(id);
    let short: String = group.chars().take(SHORT_ID_CHARS).collect();
    format!("Invocation {short}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "a5f7f3e6-4fda-4675-bc16-8ede5420b210";
    const B: &str = "0c3d9a77-14be-4f0e-9d0a-55817e2200e2";

    fn id(id: &str) -> InvocationId {
        InvocationId::new(id).expect("a non-empty id")
    }

    fn profile(name: &'static str, of: Option<&str>) -> File<&'static str> {
        File {
            file: name,
            kind: Kind::Profile,
            invocation: of.map(id),
        }
    }

    fn bep(name: &'static str, of: Option<&str>) -> File<&'static str> {
        File {
            file: name,
            kind: Kind::Bep,
            invocation: of.map(id),
        }
    }

    /// The documents `given` opens as, each written as its files' names: a
    /// invocation's as the label it opens under, then its files.
    fn grouped(given: Given<&'static str>) -> Vec<Vec<String>> {
        let (leading, others) = by_invocation(given);
        iter::once(leading)
            .chain(others)
            .map(|opening| match opening {
                Opening::Alone(name) => vec![name.to_string()],
                Opening::Invocation {
                    id,
                    first,
                    second,
                    rest,
                } => iter::once(label(&id))
                    .chain([first, second].into_iter().chain(rest).map(String::from))
                    .collect(),
            })
            .collect()
    }

    fn files(files: Vec<File<&'static str>>) -> Vec<Vec<String>> {
        let mut files = files.into_iter();
        let first = files.next().expect("a window opens at least one file");
        grouped(Given::Files {
            first,
            rest: files.collect(),
        })
    }

    fn with_graph(files: Vec<File<&'static str>>) -> Vec<Vec<String>> {
        grouped(Given::Graph { graph: "g", files })
    }

    #[test]
    fn files_naming_one_invocation_open_as_one_document() {
        assert_eq!(
            files(vec![profile("p", Some(A)), bep("b", Some(A))]),
            [["Invocation a5f7f3e6", "p", "b"]]
        );
    }

    #[test]
    fn files_naming_different_invocations_open_apart() {
        assert_eq!(
            files(vec![profile("p", Some(A)), bep("b", Some(B))]),
            [["p"], ["b"]]
        );
    }

    #[test]
    fn a_file_naming_no_invocation_opens_alone() {
        assert_eq!(
            files(vec![
                profile("p", Some(A)),
                profile("unnamed", None),
                bep("b", Some(A)),
                bep("silent", None),
            ]),
            [
                vec!["Invocation a5f7f3e6", "p", "b"],
                vec!["unnamed"],
                vec!["silent"],
            ]
        );
        assert_eq!(
            files(vec![profile("p", None), bep("b", None)]),
            [["p"], ["b"]]
        );
    }

    /// An invocation's document shows one timeline and one console, so a second
    /// profile or stream of the same invocation is a document of its own.
    #[test]
    fn an_invocation_takes_one_file_of_each_kind() {
        assert_eq!(
            files(vec![
                profile("p1", Some(A)),
                profile("p2", Some(A)),
                bep("b1", Some(A)),
                bep("b2", Some(A)),
                profile("p3", Some(A)),
            ]),
            [
                vec!["Invocation a5f7f3e6", "p1", "b1"],
                vec!["p2"],
                vec!["b2"],
                vec!["p3"],
            ]
        );
    }

    #[test]
    fn a_lone_file_opens_as_itself() {
        assert_eq!(files(vec![profile("p", Some(A))]), [["p"]]);
        assert_eq!(with_graph(Vec::new()), [["g"]]);
        assert_eq!(
            files(vec![profile("p1", Some(A)), profile("p2", Some(A))]),
            [["p1"], ["p2"]]
        );
    }

    /// A graph names no invocation, so it is taken for the one the files given
    /// with it agree on, be that a single file. A file naming none has no say.
    #[test]
    fn a_graph_joins_the_one_invocation_the_other_files_name() {
        assert_eq!(
            with_graph(vec![profile("p", Some(A)), bep("b", Some(A))]),
            [["Invocation a5f7f3e6", "g", "p", "b"]]
        );
        assert_eq!(
            with_graph(vec![bep("b", Some(A))]),
            [["Invocation a5f7f3e6", "g", "b"]]
        );
        assert_eq!(
            with_graph(vec![profile("unnamed", None), bep("b", Some(A))]),
            [vec!["Invocation a5f7f3e6", "g", "b"], vec!["unnamed"]]
        );
        assert_eq!(
            with_graph(vec![profile("p1", Some(A)), profile("p2", Some(A))]),
            [vec!["Invocation a5f7f3e6", "g", "p1"], vec!["p2"]]
        );
    }

    #[test]
    fn a_graph_opens_alone_where_the_files_name_several_invocations_or_none() {
        assert_eq!(
            with_graph(vec![profile("p", Some(A)), bep("b", Some(B))]),
            [["g"], ["p"], ["b"]]
        );
        assert_eq!(with_graph(vec![profile("p", None)]), [["g"], ["p"]]);
    }

    /// Two builds given together are two documents, each standing where its first
    /// file was given.
    #[test]
    fn documents_keep_the_order_of_their_first_file() {
        assert_eq!(
            files(vec![
                profile("pa", Some(A)),
                profile("lone", None),
                profile("pb", Some(B)),
                bep("bb", Some(B)),
                bep("ba", Some(A)),
            ]),
            [
                vec!["Invocation a5f7f3e6", "pa", "ba"],
                vec!["lone"],
                vec!["Invocation 0c3d9a77", "pb", "bb"],
            ]
        );
    }

    #[test]
    fn an_invocation_is_labelled_by_the_first_group_of_its_uuid() {
        assert_eq!(label(&id(A)), "Invocation a5f7f3e6");
    }

    /// An id is whatever the file holds, so one that is no UUID is still cut to
    /// a label's length, and one led by a hyphen still shows a group.
    #[test]
    fn an_id_that_is_no_uuid_is_labelled_by_its_leading_characters() {
        assert_eq!(label(&id("abc")), "Invocation abc");
        assert_eq!(
            label(&id("0123456789abcdef0123456789abcdef")),
            "Invocation 01234567"
        );
        assert_eq!(label(&id("-4fda-4675")), "Invocation 4fda");
        assert_eq!(label(&id("--")), "Invocation --");
    }
}
