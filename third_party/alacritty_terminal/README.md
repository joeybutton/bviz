# alacritty_terminal 0.26.0, vendored

The source of [alacritty_terminal 0.26.0](https://crates.io/crates/alacritty_terminal/0.26.0)
(Apache-2.0, see `LICENSE-APACHE`), with one change, so that bviz's console
replay builds for the browser as well as the desktop:

- `src/lib.rs` declares the `event_loop` and `tty` modules only for unix and
  windows, and `Cargo.toml` takes the crates only they use (`home`, `libc`,
  `polling`) for those targets alone. The desktop build is unchanged.

bviz uses only the emulator (`Term`, its grid, selection and search, and the
re-exported `vte` parser) to replay a recorded console; it never opens a pty.
