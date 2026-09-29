//! Why a dependency is in the build: the dependency paths from a dependent the
//! reader names, or from every dependent where none is named, down to the
//! dependency, drawn in the layered layout in place of the panel's filter.
//!
//! The mode the panel was in is the reader's own, so putting the question away
//! returns to it along with the filter.

use crate::mode::LayoutMode;
use crate::{CARD_MAX_SHARE, GraphPanel, Palette, derive};
use bviz_graph_layout::paths::{self, Paths};
use bviz_graph_layout::select::labels::carried_label;
use bviz_graph_layout::{Facets, Id, Names};
use bviz_ui_components::{Button, TextTooltip, TooltipColors, load_in_background};
use gpui::{
    ClickEvent, Context, Div, Pixels, SharedString, Stateful, Task, div, prelude::*, px, relative,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Width the card is drawn at, wide enough for a package path and a target.
const CARD_WIDTH: Pixels = px(360.);

/// Side of the mark leading each link of the shortest chain after its first.
const LINK_MARK: Pixels = px(10.);

/// Leading characters of a configuration checksum the card shows to tell apart
/// configurations sharing a mnemonic: enough to differ among the few a card
/// names, short enough to leave the target name room.
const CHECKSUM_SHOWN: usize = 6;

/// The question a panel is answering, if any.
pub(crate) struct Why {
    asked: Option<Asked>,
}

impl Why {
    pub(crate) fn new() -> Self {
        Why { asked: None }
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
/// targets of one label apart.
fn name(names: &Names, facets: &Facets, id: Id) -> Named {
    let label = carried_label(names, facets, id).unwrap_or_else(|| names.resolve(id));
    let config = facets.config(id).map(|config| Config {
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
/// followed by the start of the checksum wherever another configuration the
/// card shows shares that mnemonic.
struct Spelling {
    ambiguous: Vec<SharedString>,
}

impl Spelling {
    fn of<'a>(shown: impl Iterator<Item = &'a Named>) -> Self {
        let mut first: HashMap<&SharedString, &SharedString> = HashMap::new();
        let mut ambiguous: Vec<SharedString> = Vec::new();
        for config in shown.filter_map(|named| named.config.as_ref()) {
            let Some(mnemonic) = &config.mnemonic else {
                continue;
            };
            let seen = *first.entry(mnemonic).or_insert(&config.checksum);
            if *seen != config.checksum && !ambiguous.contains(mnemonic) {
                ambiguous.push(mnemonic.clone());
            }
        }
        Spelling { ambiguous }
    }

    fn config(&self, config: &Config) -> String {
        let short: String = config.checksum.chars().take(CHECKSUM_SHOWN).collect();
        match &config.mnemonic {
            Some(mnemonic) if self.ambiguous.contains(mnemonic) => format!("{mnemonic} {short}"),
            Some(mnemonic) => mnemonic.to_string(),
            None => short,
        }
    }

    /// `named`'s target and configuration.
    fn name(&self, named: &Named) -> SharedString {
        match &named.config {
            Some(config) => format!("{} ({})", named.target, self.config(config)).into(),
            None => named.target.clone(),
        }
    }

    /// `named`'s full label, with its configuration's whole checksum.
    fn full(&self, named: &Named) -> SharedString {
        match &named.config {
            Some(Config {
                mnemonic: Some(mnemonic),
                checksum,
            }) => format!("{} ({mnemonic}, {checksum})", named.label).into(),
            Some(Config { checksum, .. }) => format!("{} ({checksum})", named.label).into(),
            None => named.label.clone(),
        }
    }
}

/// How many nodes the paths hold, as the card states it.
fn tally(nodes: usize) -> SharedString {
    match nodes {
        1 => "1 node on the paths".into(),
        nodes => format!("{nodes} nodes on the paths").into(),
    }
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

/// The controls asking about the box a secondary click has selected: why it is
/// in the build and, while a dependency is asked about, the paths from it down
/// to that dependency. `None` while no box is selected.
pub(crate) fn controls(view: &GraphPanel, cx: &mut Context<GraphPanel>) -> Option<Div> {
    let node = view.selection.node()?;
    let asking = view
        .why
        .dependency()
        .filter(|&dependency| dependency != node);
    Some(
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(
                Button::new(
                    "why-selected",
                    cx.listener(move |view, _: &ClickEvent, _, cx| ask_why(view, node, cx)),
                )
                .ghost()
                .label("why"),
            )
            .children(asking.map(|_| {
                Button::new(
                    "from-selected",
                    cx.listener(move |view, _: &ClickEvent, _, cx| ask_from(view, node, cx)),
                )
                .ghost()
                .label("from here")
            })),
    )
}

/// One line of the card, cut short at its end where it runs past the card.
fn line(text: impl Into<SharedString>) -> Div {
    div()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_ellipsis()
        .child(text.into())
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
    let tip = TooltipColors::from_colors(&palette.colors);
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

/// The shortest chain, from the dependent down to the dependency, each link
/// after the first led by a mark reading as "which depends on".
fn chain<'a>(
    shortest: &'a [Named],
    spelling: &'a Spelling,
    palette: &'a Palette,
) -> impl Iterator<Item = Stateful<Div>> + 'a {
    let tip = TooltipColors::from_colors(&palette.colors);
    shortest.iter().enumerate().map(move |(link, named)| {
        div()
            .id(("why-link", link))
            .flex()
            .items_center()
            .gap_1()
            .min_w_0()
            .tooltip(TextTooltip::builder(spelling.full(named), tip))
            .child(
                div()
                    .size(LINK_MARK)
                    .flex_none()
                    .children((link > 0).then(|| {
                        bviz_ui_assets::icon("caret-down")
                            .size(LINK_MARK)
                            .text_color(palette.colors.text_muted)
                    })),
            )
            .child(line(spelling.name(named)))
    })
}

/// The question asked and what answers it, floating over the drawing; `None`
/// while none is asked.
pub(crate) fn card(
    view: &GraphPanel,
    palette: &Palette,
    cx: &mut Context<GraphPanel>,
) -> Option<Div> {
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
    let spelling = Spelling::of(shown);
    let from = match &question.dependent {
        Some(dependent) => endpoint("why-from", "from ", dependent, &spelling, palette),
        None => div().id("why-from").child(line("from every dependent")),
    };
    Some(
        div()
            .absolute()
            .right_2()
            .bottom_2()
            .w(CARD_WIDTH)
            .max_w(relative(CARD_MAX_SHARE))
            .occlude()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .bg(palette.container)
            .text_xs()
            .text_color(palette.label)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(line("why is this built").text_color(palette.colors.text_muted))
                    .child(
                        Button::new(
                            "why-clear",
                            cx.listener(|view, _: &ClickEvent, _, cx| clear(view, cx)),
                        )
                        .label("clear")
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
            .child(line(status))
            .children(chain(shortest, &spelling, palette)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let spelling = Spelling::of([&named].into_iter());
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

    /// Two configurations sharing a mnemonic read the same by it, so where the
    /// card shows both it tells them apart by the start of their checksums.
    #[test]
    fn configurations_sharing_a_mnemonic_are_told_apart_by_checksum() {
        let (mut names, mut facets) = (Names::default(), Facets::default());
        let one = worker(&mut names, &mut facets, "aaaaaa11", "k8-fastbuild");
        let other = worker(&mut names, &mut facets, "bbbbbb22", "k8-fastbuild");
        let alone = Spelling::of([&one].into_iter());
        assert_eq!(alone.name(&one), ":worker (k8-fastbuild)");
        let both = Spelling::of([&one, &other].into_iter());
        assert_eq!(both.name(&one), ":worker (k8-fastbuild aaaaaa)");
        assert_eq!(both.name(&other), ":worker (k8-fastbuild bbbbbb)");
    }

    #[test]
    fn the_tally_agrees_with_its_count() {
        assert_eq!(tally(1), "1 node on the paths");
        assert_eq!(tally(3), "3 nodes on the paths");
    }
}
