//! Why a dependency is in the build: the dependency paths from a dependent the
//! reader names, or from every dependent where none is named, down to the
//! dependency, drawn in the layered layout in place of the panel's filter.
//!
//! The mode the panel was in is the reader's own, so putting the question away
//! returns to it along with the filter.

use crate::column::{Folding, line, scrolling};
use crate::mode::LayoutMode;
use crate::paint::paint_diamond;
use crate::{GraphPanel, Palette, derive};
use bviz_bazel_flags::TRANSITIONS;
use bviz_graph_layout::facets::{ConfigRef, Sym};
use bviz_graph_layout::paths::{self, Paths};
use bviz_graph_layout::select::labels::{carried_label, package_label};
use bviz_graph_layout::transition::{Difference, Transition, Transitions};
use bviz_graph_layout::{Axis, Capabilities, Facets, Id, Input, Names, Support};
use bviz_ui_components::{
    Button, ListScroll, TextTooltip, TruncatingLabel, load_in_background, overlay_canvas,
};
use gpui::{ClickEvent, Context, Div, Pixels, SharedString, Stateful, Task, div, prelude::*, px};
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

/// Side of the mark leading each link of the shortest chain after its first.
const LINK_MARK: Pixels = px(10.);

/// Leading characters of a configuration checksum the card shows to tell apart
/// configurations sharing a mnemonic: enough to differ among the few a card
/// names, short enough to leave the target name room.
const CHECKSUM_SHOWN: usize = 6;

/// The question a panel is answering, if any.
pub(crate) struct Why {
    asked: Option<Asked>,
    /// How far the card's chain has scrolled.
    scroll: ListScroll,
}

impl Why {
    pub(crate) fn new() -> Self {
        Why {
            asked: None,
            scroll: ListScroll::new(),
        }
    }

    /// The dependency asked about.
    fn dependency(&self) -> Option<Id> {
        self.asked
            .as_ref()
            .map(|asked| asked.question.dependency.id)
    }
}

struct Asked {
    question: Question,
    answer: Answering,
}

#[derive(Clone)]
struct Question {
    dependency: Named,
    /// Where `None`, every dependent.
    dependent: Option<Named>,
    /// The mode the panel drew in before an answer switched it to the layered
    /// layout; `None` while none has.
    switched_from: Option<LayoutMode>,
}

enum Answering {
    /// The paths being found, the work held so that it is not cancelled.
    Pending {
        _work: Task<()>,
    },
    Answered(Answer),
}

/// A node as the card names it: by the target name and configuration that tell
/// it from its neighbours on a path, over the package holding it, with its full
/// label for the reader who needs the rest.
#[derive(Clone)]
struct Named {
    id: Id,
    target: SharedString,
    package: Option<SharedString>,
    label: SharedString,
    config: Option<Config>,
}

/// The configuration a named node is built in.
#[derive(Clone)]
struct Config {
    handle: ConfigRef,
    mnemonic: Option<SharedString>,
    checksum: SharedString,
}

enum Answer {
    Dependents {
        nodes: usize,
    },
    Between {
        nodes: usize,
        /// From the dependent down to the dependency.
        shortest: Vec<Named>,
    },
    NoPath,
}

/// How the card names `id`: by the label of the target it belongs to, and the
/// configuration it is built in where it states one, which tells the configured
/// targets of one label apart. A package box is named whole by its path.
fn name(names: &Names, facets: &Facets, id: Id) -> Named {
    if let Some(package) = package_label(names, id) {
        let package = SharedString::from(package);
        return Named {
            id,
            target: package.clone(),
            package: None,
            label: package,
            config: None,
        };
    }
    let label = carried_label(names, facets, id).unwrap_or_else(|| names.resolve(id));
    let config = facets.config(id).map(|config| Config {
        handle: config,
        mnemonic: facets
            .config_entry(config)
            .and_then(|entry| entry.mnemonic)
            .map(|mnemonic| facets.strings().resolve(mnemonic).to_string().into()),
        checksum: facets.strings().resolve(config.sym()).to_string().into(),
    });
    let (package, target) = match label.rsplit_once(':') {
        Some((package, target)) => (Some(package), format!(":{target}")),
        None => (None, label.to_string()),
    };
    Named {
        id,
        target: target.into(),
        package: package.map(|package| package.to_string().into()),
        label: label.to_string().into(),
        config,
    }
}

/// How the card writes the configurations of the nodes it shows: by mnemonic,
/// where two configurations the card shows share one marking the one carrying
/// the test options another drops, and failing that adding the start of each
/// one's checksum.
struct Spelling {
    spelled: HashMap<ConfigRef, String>,
}

/// What the card adds to a mnemonic to mark the configuration carrying the
/// test options another it shows drops.
const TEST_SPELLING: &str = "test";

impl Spelling {
    fn of<'a>(facets: &Facets, shown: impl Iterator<Item = &'a Named>) -> Self {
        let mut sharing: HashMap<Option<&SharedString>, Vec<&Config>> = HashMap::new();
        for config in shown.filter_map(|named| named.config.as_ref()) {
            let held = sharing.entry(config.mnemonic.as_ref()).or_default();
            if !held.iter().any(|held| held.handle == config.handle) {
                held.push(config);
            }
        }
        let count = sharing.values().map(Vec::len).sum();
        let mut spelled: HashMap<ConfigRef, String> = HashMap::with_capacity(count);
        for (mnemonic, configs) in sharing {
            let written: Vec<String> = match mnemonic {
                None => configs.iter().map(|config| short(config)).collect(),
                Some(mnemonic) => {
                    let marked = test_marked(facets, mnemonic, &configs);
                    let apart = marked
                        .iter()
                        .enumerate()
                        .all(|(i, one)| !marked[i + 1..].contains(one));
                    match apart {
                        true => marked,
                        false => configs
                            .iter()
                            .map(|config| format!("{mnemonic} {}", short(config)))
                            .collect(),
                    }
                }
            };
            spelled.extend(configs.iter().map(|config| config.handle).zip(written));
        }
        Spelling { spelled }
    }

    /// `named`'s target and configuration, where `named` is one of the nodes
    /// this was spelled for.
    fn name(&self, named: &Named) -> SharedString {
        match &named.config {
            Some(config) => format!("{} ({})", named.target, self.spelled[&config.handle]).into(),
            None => named.target.clone(),
        }
    }

    /// `named`'s full label, with its configuration's whole checksum.
    fn full(&self, named: &Named) -> SharedString {
        match &named.config {
            Some(Config {
                mnemonic: Some(mnemonic),
                checksum,
                ..
            }) => format!("{} ({mnemonic}, {checksum})", named.label).into(),
            Some(Config { checksum, .. }) => format!("{} ({checksum})", named.label).into(),
            None => named.label.clone(),
        }
    }
}

/// What the card says of the transitions the drawing cannot mark, where the
/// inputs leave some edges without the configuration of their dependency, or
/// hold no configured target at all where `configured` is false.
fn unmarked_transitions(capabilities: &Capabilities, configured: bool) -> Option<SharedString> {
    if !configured {
        return Some(
            format!("transitions unmarked — they need a cquery run with {TRANSITIONS}").into(),
        );
    }
    let unmarked = match capabilities.support(Axis::ExecConfiguration) {
        Support::Available => return None,
        Support::Partial { .. } => "some transitions unmarked",
        Support::Unavailable { .. } => "transitions unmarked",
    };
    Some(format!("{unmarked} — run the cquery with {TRANSITIONS}").into())
}

/// How many changed options a link's hover names before it counts the rest.
const CHANGES_SHOWN: usize = 6;

/// The longest option value a link's hover writes out, beyond which it stands
/// the value in by an ellipsis: a platform label fits, a list of flags does not.
const VALUE_SHOWN: usize = 40;

/// What a link crossing `transition` from `dependent` to `dependency` says of
/// it on hover: the kind and, for a platform or a flag transition or a target
/// taken in several configurations, the options it changes, each from the
/// dependent's value to the dependency's.
fn crossing(
    facets: &Facets,
    transition: Transition,
    dependency: &Named,
    dependent: &Named,
) -> String {
    let mut text = transition.label().to_string();
    let lists_options = matches!(
        transition,
        Transition::Platform | Transition::Flag | Transition::Split
    );
    let (true, Some(to), Some(from)) = (lists_options, &dependency.config, &dependent.config)
    else {
        return text;
    };
    let Some(difference) = Difference::between(facets, to.handle, from.handle) else {
        return text;
    };
    let value = |value: Option<Sym>| match value.map(|sym| facets.strings().resolve(sym)) {
        None => "unset",
        Some("") => "\"\"",
        Some(value) if value.chars().count() <= VALUE_SHOWN => value,
        Some(_) => "…",
    };
    for change in difference.changes.iter().take(CHANGES_SHOWN) {
        let name = facets.strings().resolve(change.name);
        let (was, is) = (value(change.dependent), value(change.dependency));
        let _ = write!(text, "\n{name}: {was} → {is}");
    }
    let beyond = difference.changes.len().saturating_sub(CHANGES_SHOWN);
    if beyond > 0 {
        let _ = write!(text, "\n+{beyond} more");
    }
    text
}

/// One link of the shortest chain, with the transition the drawing marks on
/// the edge it follows and what its hover says.
struct Link<'a> {
    named: &'a Named,
    crossing: Option<Transition>,
    hover: SharedString,
}

/// The start of `config`'s checksum the card tells configurations apart by.
fn short(config: &Config) -> String {
    config.checksum.chars().take(CHECKSUM_SHOWN).collect()
}

/// `configs`, which share `mnemonic`, each spelled by it and marked where it
/// carries the test options another of them drops and differs from it by
/// nothing else.
fn test_marked(facets: &Facets, mnemonic: &str, configs: &[&Config]) -> Vec<String> {
    configs
        .iter()
        .map(|config| {
            let tested = configs.iter().any(|other| {
                other.handle != config.handle
                    && Difference::between(facets, other.handle, config.handle).is_some_and(
                        |difference| difference.drops_test_options && difference.is_empty(),
                    )
            });
            match tested {
                true => format!("{mnemonic}, {TEST_SPELLING}"),
                false => mnemonic.to_string(),
            }
        })
        .collect()
}

/// How many nodes the paths hold, as the card states it.
fn tally(nodes: usize) -> SharedString {
    match nodes {
        1 => "1 node on the paths".into(),
        nodes => format!("{nodes} nodes on the paths").into(),
    }
}

/// Whether the question open is about a dependency other than `node`, which
/// [`ask_from`] can narrow to the paths from it.
pub(crate) fn asking_about_another(view: &GraphPanel, node: Id) -> bool {
    view.why
        .dependency()
        .is_some_and(|dependency| dependency != node)
}

/// Asks why `dependency` is in the build, keeping the dependent already named.
pub(crate) fn ask_why(view: &mut GraphPanel, dependency: Id, cx: &mut Context<GraphPanel>) {
    let dependent = view
        .why
        .asked
        .as_ref()
        .and_then(|asked| asked.question.dependent.as_ref())
        .map(|dependent| dependent.id)
        .filter(|&dependent| dependent != dependency);
    ask(view, dependency, dependent, cx);
}

/// Narrows the question asked to the paths from `dependent`.
pub(crate) fn ask_from(view: &mut GraphPanel, dependent: Id, cx: &mut Context<GraphPanel>) {
    if let Some(dependency) = view.why.dependency() {
        ask(view, dependency, Some(dependent), cx);
    }
}

/// Asks for the dependency paths from `dependent` down to `dependency`.
pub(crate) fn ask_between(
    view: &mut GraphPanel,
    dependency: Id,
    dependent: Id,
    cx: &mut Context<GraphPanel>,
) {
    ask(view, dependency, Some(dependent), cx);
}

/// Whether the question open is the one [`ask_between`] asks of these two.
pub(crate) fn asking_between(view: &GraphPanel, dependency: Id, dependent: Id) -> bool {
    view.why.asked.as_ref().is_some_and(|asked| {
        let question = &asked.question;
        let from = question.dependent.as_ref().map(|dependent| dependent.id);
        question.dependency.id == dependency && from == Some(dependent)
    })
}

fn ask(view: &mut GraphPanel, dependency: Id, dependent: Option<Id>, cx: &mut Context<GraphPanel>) {
    let Some(shared) = view.model.read(cx).loaded().map(|l| &l.shared) else {
        return;
    };
    let (base, names, facets) = (
        Arc::clone(&shared.base),
        Arc::clone(&shared.names),
        Arc::clone(&shared.facets),
    );
    let question = Question {
        dependency: name(&names, &facets, dependency),
        dependent: dependent.map(|dependent| name(&names, &facets, dependent)),
        switched_from: view
            .why
            .asked
            .as_ref()
            .and_then(|asked| asked.question.switched_from),
    };
    let answering = question.clone();
    let work = load_in_background(
        cx,
        move || match dependent {
            None => {
                let paths = paths::dependents(&base, dependency);
                let nodes = paths.nodes.len();
                Some((paths, Answer::Dependents { nodes }))
            }
            Some(dependent) => {
                let between = paths::between(&base, dependency, dependent)?;
                let shortest = between
                    .shortest
                    .iter()
                    .rev()
                    .map(|&id| name(&names, &facets, id))
                    .collect();
                let nodes = between.paths.nodes.len();
                Some((between.paths, Answer::Between { nodes, shortest }))
            }
        },
        move |view, found: Option<(Paths, Answer)>, cx| {
            let mut question = answering;
            match found {
                Some((paths, answer)) => {
                    question.switched_from.get_or_insert(view.mode);
                    view.why.asked = Some(Asked {
                        question,
                        answer: Answering::Answered(answer),
                    });
                    view.switch_mode(LayoutMode::Layered, cx);
                    derive::set_paths(view, Some(Arc::new(paths)), cx);
                }
                None => {
                    restore_mode(view, question.switched_from.take(), cx);
                    view.why.asked = Some(Asked {
                        question,
                        answer: Answering::Answered(Answer::NoPath),
                    });
                    derive::set_paths(view, None, cx);
                }
            }
            derive::ensure_layout(view, cx);
            view.invalidate_canvas(cx);
        },
    );
    view.why.asked = Some(Asked {
        question,
        answer: Answering::Pending { _work: work },
    });
    cx.notify();
}

/// Returns the panel to `switched_from`, the mode an answer switched it out of,
/// unless the reader has since left the layered layout it switched to.
fn restore_mode(
    view: &mut GraphPanel,
    switched_from: Option<LayoutMode>,
    cx: &mut Context<GraphPanel>,
) {
    if let Some(mode) = switched_from
        && view.mode == LayoutMode::Layered
    {
        view.switch_mode(mode, cx);
    }
}

/// Puts the question away, and the work answering it with it, and returns the
/// panel to the mode an answer switched it out of, leaving the caller to derive
/// what the panel draws in place of the paths.
pub(crate) fn put_away(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    let Some(asked) = view.why.asked.take() else {
        return;
    };
    restore_mode(view, asked.question.switched_from, cx);
    cx.notify();
}

/// Puts the question away and draws what the panel drew before it.
pub(crate) fn clear(view: &mut GraphPanel, cx: &mut Context<GraphPanel>) {
    put_away(view, cx);
    derive::set_paths(view, None, cx);
    derive::ensure_layout(view, cx);
    view.invalidate_canvas(cx);
}

/// Forgets the question, which named nodes of a graph that has been replaced.
pub(crate) fn reset(view: &mut GraphPanel) {
    view.why = Why::new();
}

/// An endpoint of the question: its name after `lead`, over the package holding
/// it, with its full label on hover.
fn endpoint(
    key: &'static str,
    lead: &str,
    named: &Named,
    spelling: &Spelling,
    palette: &Palette,
) -> Stateful<Div> {
    let tip = palette.colors.tooltip();
    div()
        .id(key)
        .flex()
        .flex_col()
        .min_w_0()
        .tooltip(TextTooltip::builder(spelling.full(named), tip))
        .child(line(format!("{lead}{}", spelling.name(named))))
        .children(named.package.clone().map(|package| {
            // Packages sharing a prefix differ at their tails.
            line(package)
                .text_ellipsis_start()
                .text_color(palette.colors.text_muted)
        }))
}

/// The shortest chain's links, from the dependent down to the dependency,
/// with the transitions `transitions` marks between them, their hovers read
/// off `facets`.
fn links<'a>(
    shortest: &'a [Named],
    transitions: &Transitions,
    facets: &Facets,
    spelling: &Spelling,
) -> Vec<Link<'a>> {
    let above = std::iter::once(None).chain(shortest.iter().map(Some));
    shortest
        .iter()
        .zip(above)
        .map(|(named, dependent)| {
            let crossed = dependent.and_then(|dependent| {
                let transition = transitions.get(named.id, dependent.id)?;
                Some((transition, crossing(facets, transition, named, dependent)))
            });
            let full = spelling.full(named);
            let (crossing, hover) = match crossed {
                Some((transition, crossing)) => {
                    (Some(transition), format!("{full}\n{crossing}").into())
                }
                None => (None, full),
            };
            Link {
                named,
                crossing,
                hover,
            }
        })
        .collect()
}

/// The shortest chain, each link after the first led by a mark reading as
/// "which depends on": the badge of the transition the drawing marks on the
/// edge the link follows, where it marks one.
fn chain<'a>(
    links: &'a [Link<'a>],
    spelling: &'a Spelling,
    palette: &'a Palette,
) -> impl Iterator<Item = Stateful<Div>> + 'a {
    let tip = palette.colors.tooltip();
    links.iter().enumerate().map(move |(position, link)| {
        div()
            .id(("why-link", position))
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .tooltip(TextTooltip::builder(link.hover.clone(), tip))
            .child(
                div()
                    .size(LINK_MARK)
                    .flex_none()
                    .relative()
                    .children(link.crossing.map(|transition| {
                        let color = palette.transition_color(transition);
                        overlay_canvas(move |bounds, window, _| {
                            paint_diamond(bounds.center(), f32::from(LINK_MARK) / 2., color, window)
                        })
                        .into_any_element()
                    }))
                    .children((position > 0 && link.crossing.is_none()).then(|| {
                        bviz_ui_assets::icon("arrow-down")
                            .size(LINK_MARK)
                            .text_color(palette.colors.text_muted)
                            .into_any_element()
                    })),
            )
            .child(line(spelling.name(link.named)).flex_1())
    })
}

/// The links of the chain.
fn listed(
    view: &GraphPanel,
    links: impl Iterator<Item = Stateful<Div>>,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> Div {
    let rows = div()
        .id("why-chain")
        .flex()
        .flex_col()
        .gap_1()
        .overflow_y_scroll()
        .track_scroll(view.why.scroll.handle())
        .children(links);
    scrolling(
        rows,
        |view| &view.why.scroll,
        |view| &mut view.why.scroll,
        palette,
        cx,
    )
}

/// The question asked and what answers it, for the caller to place over the
/// drawing; `None` while none is asked.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &Context<GraphPanel>,
) -> Option<Folding> {
    let Asked { question, answer } = view.why.asked.as_ref()?;
    let (status, shortest): (SharedString, &[Named]) = match answer {
        Answering::Pending { .. } => ("finding paths…".into(), &[]),
        Answering::Answered(Answer::NoPath) => ("no path".into(), &[]),
        Answering::Answered(Answer::Dependents { nodes }) => (tally(*nodes), &[]),
        Answering::Answered(Answer::Between { nodes, shortest }) => (tally(*nodes), shortest),
    };
    let shown = [&question.dependency]
        .into_iter()
        .chain(&question.dependent)
        .chain(shortest);
    let shared = &view.model.read(cx).loaded()?.shared;
    let spelling = Spelling::of(&shared.facets, shown);
    let found = matches!(
        answer,
        Answering::Answered(Answer::Dependents { .. } | Answer::Between { .. })
    );
    let unmarked = found
        .then(|| {
            let configured = [Input::Cquery, Input::Aquery]
                .into_iter()
                .any(|input| shared.inputs.contains(input));
            unmarked_transitions(&shared.capabilities, configured)
        })
        .flatten();
    let links = links(
        shortest,
        &view.derived.transitions(),
        &shared.facets,
        &spelling,
    );
    // A map level draws the reduction of what it holds and folds edges into
    // its boxes, so it marks the transitions only of the edges it draws.
    let map_note = (found && view.mode == LayoutMode::Map)
        .then_some("the map leaves some transitions unmarked; the layered layout marks each");
    let from = match &question.dependent {
        Some(dependent) => endpoint("why-from", "from ", dependent, &spelling, palette),
        None => div()
            .id("why-from")
            .child(TruncatingLabel::new("label", "from every dependent")),
    };
    let head = div()
        .flex()
        .flex_col()
        .gap_1()
        .text_xs()
        .text_color(palette.label)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    TruncatingLabel::new("why-heading", "why is this built")
                        .text_color(palette.colors.text_muted),
                )
                .child(
                    Button::new(
                        "why-clear",
                        cx.listener(|view, _: &ClickEvent, _, cx| clear(view, cx)),
                    )
                    .label("clear")
                    .tooltip("Clear paths")
                    .compact(),
                ),
        )
        .child(endpoint(
            "why-dependency",
            "",
            &question.dependency,
            &spelling,
            palette,
        ))
        .child(from)
        .child(TruncatingLabel::new("why-status", status))
        .children(unmarked.map(|unmarked| div().min_w_0().child(unmarked)))
        .children(map_note.map(|note| div().min_w_0().child(note)))
        .children((!links.is_empty()).then(|| {
            TruncatingLabel::new("why-shortest", "shortest:").text_color(palette.colors.text_muted)
        }));
    let chain =
        (!links.is_empty()).then(|| listed(view, chain(&links, &spelling, palette), palette, cx));
    Some(Folding::new(head, chain, palette))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::capability::CapabilitiesBuilder;
    use bviz_graph_layout::facets::Tri;

    /// The configured target `worker` in the configuration checksummed
    /// `checksum` under `mnemonic`.
    fn worker(names: &mut Names, facets: &mut Facets, checksum: &str, mnemonic: &str) -> Named {
        let target = names.intern("target://src/main/java/com/example:worker");
        let configured = names.intern(&format!("ct://src/main/java/com/example:worker#{checksum}"));
        facets.set_label(target, target);
        facets.set_label(configured, target);
        let config = facets.intern_config(checksum, Some(mnemonic), Tri::No);
        facets.set_config(configured, Some(config));
        name(names, facets, configured)
    }

    #[test]
    fn a_node_is_named_by_its_target_and_configuration_over_its_package() {
        let (mut names, mut facets) = (Names::default(), Facets::default());
        let named = worker(&mut names, &mut facets, "0123456789", "k8-opt-exec");
        let spelling = Spelling::of(&facets, [&named].into_iter());
        assert_eq!(spelling.name(&named), ":worker (k8-opt-exec)");
        assert_eq!(
            named.package.as_deref(),
            Some("//src/main/java/com/example")
        );
        assert_eq!(
            spelling.full(&named),
            "//src/main/java/com/example:worker (k8-opt-exec, 0123456789)"
        );

        let action = names.intern("action7");
        let unlabelled = name(&names, &facets, action);
        assert_eq!(
            (spelling.name(&unlabelled).as_ref(), unlabelled.package),
            ("action7", None)
        );
    }

    #[test]
    fn a_package_is_named_whole_by_its_path() {
        let (mut names, facets) = (Names::default(), Facets::default());
        let package = names.intern("pkg:ui/components");
        let named = name(&names, &facets, package);
        let spelling = Spelling::of(&facets, [&named].into_iter());
        assert_eq!(spelling.name(&named), "//ui/components");
        assert_eq!(named.package, None);
        assert_eq!(spelling.full(&named), "//ui/components");
    }

    /// Two configurations sharing a mnemonic read the same by it, so where the
    /// card shows both it tells them apart by the start of their checksums.
    #[test]
    fn configurations_sharing_a_mnemonic_are_told_apart_by_checksum() {
        let (mut names, mut facets) = (Names::default(), Facets::default());
        let one = worker(&mut names, &mut facets, "aaaaaa11", "k8-fastbuild");
        let other = worker(&mut names, &mut facets, "bbbbbb22", "k8-fastbuild");
        let alone = Spelling::of(&facets, [&one].into_iter());
        assert_eq!(alone.name(&one), ":worker (k8-fastbuild)");
        let both = Spelling::of(&facets, [&one, &other].into_iter());
        assert_eq!(both.name(&one), ":worker (k8-fastbuild aaaaaa)");
        assert_eq!(both.name(&other), ":worker (k8-fastbuild bbbbbb)");
    }

    /// The configured target `worker` in a `k8-fastbuild` configuration
    /// checksummed `checksum`, carrying the test options where `test`, and set
    /// to `mode`.
    fn stated(
        names: &mut Names,
        facets: &mut Facets,
        checksum: &str,
        test: bool,
        mode: &str,
    ) -> Named {
        let named = worker(names, facets, checksum, "k8-fastbuild");
        let config = named.config.as_ref().expect("a configuration").handle;
        let core = ("CoreOptions", vec![("compilation_mode", Some(mode))]);
        let tests = (
            "TestConfiguration$TestOptions",
            vec![("test_timeout", Some("60"))],
        );
        let classes = match test {
            true => vec![core, tests],
            false => vec![core],
        };
        facets.record_config_options(config, classes);
        named
    }

    /// A test's configuration and its dependency's, which differ only by the
    /// test options Bazel drops, are told apart by naming the test's.
    #[test]
    fn the_configuration_carrying_the_test_options_is_named_for_them() {
        let (mut names, mut facets) = (Names::default(), Facets::default());
        let test = stated(&mut names, &mut facets, "aaaaaa11", true, "fastbuild");
        let trimmed = stated(&mut names, &mut facets, "bbbbbb22", false, "fastbuild");
        let spelling = Spelling::of(&facets, [&test, &trimmed].into_iter());
        assert_eq!(spelling.name(&test), ":worker (k8-fastbuild, test)");
        assert_eq!(spelling.name(&trimmed), ":worker (k8-fastbuild)");

        let other = stated(&mut names, &mut facets, "cccccc33", false, "opt");
        let spelling = Spelling::of(&facets, [&test, &trimmed, &other].into_iter());
        assert_eq!(
            [&test, &trimmed, &other].map(|named| spelling.name(named)),
            [
                ":worker (k8-fastbuild aaaaaa)",
                ":worker (k8-fastbuild bbbbbb)",
                ":worker (k8-fastbuild cccccc)",
            ],
            "configurations the test options do not tell apart fall back to checksums"
        );
    }

    fn observed(resolved: &[bool]) -> Capabilities {
        let mut capabilities = CapabilitiesBuilder::default();
        for &resolved in resolved {
            capabilities.observe(Axis::ExecConfiguration, resolved);
        }
        capabilities.seal()
    }

    #[test]
    fn inputs_stating_every_dependency_configuration_mark_every_transition() {
        assert_eq!(unmarked_transitions(&observed(&[true, true]), true), None);
        assert_eq!(unmarked_transitions(&observed(&[]), true), None);
    }

    #[test]
    fn inputs_lacking_dependency_configurations_name_the_flag_supplying_them() {
        assert_eq!(
            unmarked_transitions(&observed(&[false, false]), true).as_deref(),
            Some("transitions unmarked — run the cquery with --transitions=lite")
        );
        assert_eq!(
            unmarked_transitions(&observed(&[true, false]), true).as_deref(),
            Some("some transitions unmarked — run the cquery with --transitions=lite")
        );
    }

    /// A query states no configuration at all, so a graph read from one alone
    /// holds no configured target for a transition to run between.
    #[test]
    fn inputs_holding_no_configured_target_need_a_cquery() {
        assert_eq!(
            unmarked_transitions(&observed(&[]), false).as_deref(),
            Some("transitions unmarked — they need a cquery run with --transitions=lite")
        );
    }

    /// A flag transition names the options it changes on hover, each from the
    /// dependent's value to the dependency's: an empty value quoted, an option
    /// set to nothing as unset, and a value too long to read at a glance
    /// standing in by an ellipsis.
    #[test]
    fn a_crossing_names_the_options_it_changes() {
        let mut names = Names::default();
        let mut facets = Facets::default();
        let mut node = |name: &str, checksum: &str, mode: &str, copts: &str, wrap: Option<&str>| {
            let id = names.intern(name);
            let config = facets.intern_config(checksum, Some(mode), Tri::No);
            let options = [
                ("compilation_mode", Some(mode)),
                ("copt", Some(copts)),
                ("run_under", wrap),
            ];
            facets.record_config_options(config, [("CoreOptions", options)]);
            facets.set_config(id, Some(config));
            id
        };
        let long = "-O2 ".repeat(VALUE_SHOWN);
        let (app, tool) = (
            node("app", "a", "fastbuild", "", None),
            node("tool", "b", "opt", &long, Some("//:wrap")),
        );
        let (app, tool) = (name(&names, &facets, app), name(&names, &facets, tool));

        assert_eq!(
            crossing(&facets, Transition::Flag, &tool, &app),
            "flag transition\ncompilation_mode: fastbuild → opt\ncopt: \"\" → …\nrun_under: unset → //:wrap"
        );
        assert_eq!(
            crossing(&facets, Transition::Exec, &tool, &app),
            "exec transition"
        );
    }

    #[test]
    fn the_tally_agrees_with_its_count() {
        assert_eq!(tally(1), "1 node on the paths");
        assert_eq!(tally(3), "3 nodes on the paths");
    }
}
