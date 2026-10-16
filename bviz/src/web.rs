//! bviz in a browser: the same window, its command line given by the page.
//!
//! A browser has neither a command line nor a filesystem, so the page that
//! loads bviz stands in for both. It hands over each file the user picks,
//! drops or links by the name a command line would give it ([`provide_file`]),
//! then starts bviz with a command line naming them ([`start`]). The command
//! line goes through the desktop app's own parser and the window opens
//! through its own `open_window`, so the browser shows what the desktop does
//! for the same files and flags.

use super::{Cli, Request, open_window};
use clap::Parser;
use wasm_bindgen::prelude::*;

/// Makes `bytes` the contents of the file a command line names `name`.
#[wasm_bindgen]
pub fn provide_file(name: String, bytes: Vec<u8>) {
    bviz_files::provide(name, bytes);
}

/// What `bviz bazelrc` writes, for the page to show: the command line
/// `args` (without the program name) parsed as the desktop app parses it.
/// Fails with clap's own message where they are no bazelrc command line.
#[wasm_bindgen]
pub fn bazelrc(args: Vec<String>) -> Result<String, JsError> {
    match parse(args)? {
        Request::Bazelrc(args) => Ok(args.bazelrc()),
        Request::Open(_) => Err(JsError::new("not a `bviz bazelrc` command line")),
    }
}

/// Opens bviz's window on the page's canvas for the command line `args`
/// (without the program name), whose files were handed over with
/// [`provide_file`]. Fails with clap's own usage message for a command line
/// the desktop app would refuse. Call it once: the window is the page's.
#[wasm_bindgen]
pub fn start(args: Vec<String>) -> Result<(), JsError> {
    match parse(args)? {
        Request::Open(sources) => {
            gpui_platform::web_init();
            // The page's one thread becomes the worker of bviz's pool before
            // anything else claims it, so every parallel pass, nucleo's among
            // them, runs in that pool (see util/parallel).
            bviz_parallel::workers();
            open_window(sources);
            Ok(())
        }
        Request::Bazelrc(_) => Err(JsError::new(
            "`bviz bazelrc` writes a file rather than opening a window; call bazelrc",
        )),
    }
}

fn parse(args: Vec<String>) -> Result<Request, JsError> {
    Cli::try_parse_from(std::iter::once("bviz".to_string()).chain(args))
        .and_then(Cli::request)
        .map_err(|e| JsError::new(&e.render().to_string()))
}
