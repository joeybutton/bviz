use crate::summary::Summary;
use bviz_ui_components::{Pending, load_in_background};
use gpui::{Context, Task};

pub(crate) type Source = bviz_ui_components::Source<Summary>;

/// The summary of an execution log, held apart from the panel showing it so
/// the panel can close and be shown again without the log being read afresh.
pub(crate) struct ExecLogModel {
    source: Source,
    /// The read that fills `source`, held so that it is not cancelled.
    _load: Task<()>,
}

impl ExecLogModel {
    pub(crate) fn reading(log: Pending<Summary>, cx: &mut Context<Self>) -> Self {
        Self {
            source: Source::Loading,
            _load: load_in_background(
                cx,
                move || log.join(),
                |model, result, _| {
                    model.source = match result {
                        Ok(log) => Source::Loaded(log),
                        Err(why) => Source::Failed(why),
                    };
                },
            ),
        }
    }

    pub(crate) fn source(&self) -> &Source {
        &self.source
    }
}
