# The scroll model: what owns the position

Read this before adding or changing a scroll bar, or before touching a stored
scroll offset. The short version lives in SKILL.md; this is the reasoning and
the evidence behind it.

## What gpui actually gives you

`ScrollHandle` is an `Rc<RefCell<ScrollHandleState>>` shared with the div's
interactivity, and it already holds everything a bar needs:

- `offset()` — the content's top-left relative to the container, so it is `≤ 0`
  and grows more negative as you scroll down.
- `max_offset()` — the positive magnitude, `padded_content − bounds`.
- `bounds()` — the viewport it was laid out in.

`max_offset` and `bounds` are recomputed into the handle **every prepaint**
(`Interactivity::clamp_scroll_position`), which also **re-clamps the offset**.
`set_offset` itself does not clamp — the next prepaint does. The wheel handler
adds a pixel delta straight to the offset and notifies; no scroll bar is
involved at any point. `uniform_list` routes its `base_handle` through the same
interactivity, so all three accessors are live there too.

The consequence worth internalizing: for anything backed by a handle you already
have the model. Mirroring it on the view is duplication, not caching.

## Three questions a scroll model must answer

Settle these before designing one; they are what separate the shapes a scroll
model can take.

1. **Who owns the offset?** For anything backed by a gpui handle, the handle
   does — pixels in one shared cell, clamped every prepaint. For a
   custom-painted panel the view owns it, and must hold it unclamped and bound
   it at the read; see the rules below.
2. **Does the bar own anything?** It must not. A bar that asks its handle for
   the extents and the offset every frame cannot disagree with what actually
   scrolls; a bar that caches them can, and does, the first time content
   reflows between frames. Geometry the bar itself persists is for hit-testing
   only, and every click and drag recomputes the offset from the live extents
   and writes it back through the one setter.
3. **What unit survives a reflow?** Not pixels. Content that reflows — rows
   collapsing, filtering, reordering, wrapping — needs an anchor in *content*
   units: an identity into the content plus a sub-unit offset. Every mutation
   then funnels through one setter, which is the only place clamping policy
   lives, and wheel, keyboard and the bar are all just callers of it.

## Rules that follow

### Derive the bar; never store it

The offset is the model and a bar is a rendering of it. Storing an
`Option<Scrollbar>` on the view and writing it back from inside the paint closure
gives you two representations of one fact, and hit-tests presses against a rect
from a frame that may since have been *replayed* rather than repainted (see
`.cached()` in SKILL.md). Build the bar in one accessor that both the paint and
the mouse handlers call, so exactly one place knows each bar's track rect and
extents. `ui/graph/src/scroll.rs` (`scrollbar_geom` / `current_scrollbar`) is the
original; every trace panel now follows it.

For a list, derive straight from the handle — `bounds()`, `max_offset()`,
`offset()`. Asking the handle every frame is what keeps the bar from
disagreeing with what actually scrolls.

### Hold the position unclamped; bound it where it is used

A ceiling that moves — a resize, a reflow, a shorter document — must not rewrite
stored state. Clamping into the field destroys what it truncates, so growing a
window and shrinking it back does not return the view, and a rebuild that shortens
the content silently discards where the user was. Keep the held value, and bound
it at the read, in one accessor every reader goes through: the paint, the
hit-tests, any cull-cache key, and the bar's extents. Miss one and what is drawn
and what is clicked disagree.

gpui's own div clamps in prepaint, so a plain `ScrollHandle` is *not*
resize-invertible. That is fine for a list, and wrong for a custom-painted panel
where the user has a place they were looking at.

### Bound a gesture's target before deciding it moved

This one shipped twice here and was caught twice in review, in both panels:

```rust
fn drawn_scroll(held: f32, ceiling: f32) -> f32 { held.clamp(0.0, ceiling) }

fn scroll_request(held: f32, ceiling: f32, request: f32) -> Option<f32> {
    let target = drawn_scroll(request, ceiling);
    (target != drawn_scroll(held, ceiling)).then_some(target)
}
```

Comparing an *unclamped* request against the *clamped* drawn offset reduces the
guard to "was the delta non-zero", so it never fires for a real gesture: a wheel
notch at the bottom that moves nothing on screen overwrites the held position —
precisely the failure the unclamped hold exists to prevent. It also mints a
phantom overscroll when the position is already at the ceiling, which then jumps
the view the next time the content grows. Both panels share
`drawn_scroll`/`scroll_request` in `ui/trace/src/lib.rs`; the rule is tested once,
there.

### If the content reflows, anchor to a row rather than a pixel

Pixels survive a resize and nothing else. When rows appear, disappear or reorder
— a collapse, a filter, a visibility toggle — store **which row is at the top of
the viewport** and how far into it the top edge sits, then derive pixels per
frame:

- Identify a row by a **document identity** (a track index into the profile's
  threads, a group's index in a fixed order) — never by its position in the
  laid-out vec, which moves with visibility, collapse and activity sorting.
- Make resolution **total**. Define what a row the layout no longer draws
  resolves to — the lane that took over its rank in its group, then the group's
  band, then the nearest preceding row, then the start — rather than guarding a
  case you believe cannot happen. An anchor into reflowing content needs a
  stated bias for exactly this reason: the position it names can fall between
  two units after an edit, and which side it lands on has to be a decision.
  Landing on the group header instead of the neighbouring lane is a visible
  regression: hiding one track should shift the view by a row, not by a screen.
- **Round-trip exactly**: `offset_of(anchor_at(y)) == y` for every `y`, including
  a `y` that falls in the *gap* between two rows. Choose a representation that can
  express a gap position (pixels into the row's slot, where the slot is the row
  plus its trailing gap) instead of one that silently snaps.
- **Let the anchor point past the end.** That is what makes shrink-then-grow
  return the view; only the derived pixel value is bounded.
- Keep the mapping a **pure function over the row sequence**, so it is
  unit-testable without a `Window`/`App` — otherwise scroll behaviour is
  effectively untestable, which is how both guard bugs above survived to review.

Uniform rows do not need any of this. If every row is the same fixed height,
`depth × height` *is* the pixel offset, and an anchor type buys nothing over an
unclamped `f32`. Adding one for symmetry with a sibling panel is a cost with no
return — say so and keep the simpler shape.

## Verifying a scroll change

A static screenshot proves the frame, not the behaviour. Pixel-identical captures
across a refactor are strong evidence that nothing moved; they say nothing about
whether a position survives a reflow.

Headless `cage` cannot inject input, so drive the sequence programmatically with
the *same* instrumentation on both revisions and take **three** captures:
control (seek, no perturbation), old-revision cycle (seek → perturb → restore),
new-revision cycle. The claim holds when the new cycle matches the control and
the old one does not — two captures would only show that the revisions differ.

Two details that decide whether the experiment says anything:

- **Diff a region that isolates the axis.** A gutter or label column depends only
  on vertical position, so a zero-differing-pixel result there survives unrelated
  differences elsewhere in the frame.
- **Pick a perturbation that actually moves the ceiling.** For a panel whose
  content dwarfs its viewport, resizing the panel will not; collapsing groups
  will. A lever that does not move the bound validates nothing.
