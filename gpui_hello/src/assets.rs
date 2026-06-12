//! Serves the bundled Phosphor icon SVGs to gpui. The files are downloaded by a
//! Bazel `http_archive` and read from runfiles on demand; `svg().path(..)` asks
//! this source for the bytes and tints the result with the element's text color.

use gpui::{AssetSource, SharedString, Svg, svg};
use runfiles::{Runfiles, rlocation};
use std::borrow::Cow;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let Ok(runfiles) = Runfiles::create() else {
            return Ok(None);
        };
        let Some(resolved) = rlocation!(runfiles, format!("phosphor/{path}")) else {
            return Ok(None);
        };
        Ok(Some(std::fs::read(resolved)?.into()))
    }

    fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

/// A regular-weight Phosphor icon, tinted by the element's text color.
pub fn icon(name: &str) -> Svg {
    svg().path(format!("assets/regular/{name}.svg"))
}
