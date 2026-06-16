//! A helper for running work off the UI thread and applying its result.

use gpui::{Context, Task};

/// Run `work` on the background executor, then apply its result to the view on
/// the UI thread and request a repaint. Returns the [`Task`]; keep it alive
/// (store it in a field) or the work is cancelled when the task drops.
pub fn load_in_background<V, T, W, A>(cx: &mut Context<V>, work: W, apply: A) -> Task<()>
where
    V: 'static,
    T: Send + 'static,
    W: FnOnce() -> T + Send + 'static,
    A: FnOnce(&mut V, T, &mut Context<V>) + 'static,
{
    cx.spawn(async move |this, cx| {
        let result = cx.background_executor().spawn(async move { work() }).await;
        let _ = this.update(cx, |view, cx| {
            apply(view, result, cx);
            cx.notify();
        });
    })
}
