# Wheel modifiers and window focus on Wayland

**Wheel modifiers need window focus — and on Wayland an unfocused window CANNOT get them.**
`wl_pointer.axis` (scroll) carries no modifier field; modifier state rides only on
`wl_keyboard.modifiers`, which a compositor sends only to the keyboard-focused surface. So
inside `on_scroll_wheel` an unfocused window sees `event.modifiers` empty, and `Ctrl/Shift+wheel`
zoom/pan do nothing. `window.modifiers()` is no better (gpui overwrites it from the same scroll
event), nor is a `ModifiersChanged` tracker (those events also stop when unfocused). This is NOT
recoverable client-side: Mutter explicitly stopped leaking modifier state to non-focused surfaces
(GNOME/mutter MR !3704), and `window.activate_window()` (xdg_activation_v1) is rejected by GNOME's
focus-stealing prevention without a valid recent-interaction serial — so activating on hover is a
no-op there (do NOT add it; it's dead code with a false premise). What DOES work: GNOME's
click-to-focus already focuses the window on a click, so Ctrl/Shift+wheel works after one click;
focus-follows-mouse (a user setting the app can't change) makes it work on hover. For an affordance
that works while unfocused, use pure-pointer controls (on-canvas/toolbar +/- zoom buttons) — those
need no modifier. Plain-wheel scroll always works unfocused; only modifier-gated gestures are
affected. Evidence: wayland-book seat/pointer + seat/keyboard; gpui wayland `client.rs` builds
`ScrollWheelEvent.modifiers` from the last `wl_keyboard.modifiers` only.
