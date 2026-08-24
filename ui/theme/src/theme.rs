use gpui::{Hsla, SharedString};

/// Whether a theme is meant for a light or a dark system appearance.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Appearance {
    Light,
    Dark,
}

/// A named set of colors the whole UI paints from.
pub struct Theme {
    pub name: SharedString,
    pub appearance: Appearance,
    pub colors: ThemeColors,
    pub data: DataColors,
    pub terminal: TerminalColors,
}

/// The semantic roles every view paints with.
///
/// The surfaces are opaque and every layer washed over them is not, so one layer
/// marks a control the same way wherever that control sits and whatever lies
/// under the layer still reads through it.
#[derive(Clone, Copy)]
pub struct ThemeColors {
    /// The canvas a view lays its content out on.
    pub background: Hsla,
    /// The bands and bars framing the canvas.
    pub surface: Hsla,
    /// The sheet a transient view floats over the rest on.
    pub panel: Hsla,
    /// The lighter of two neutral washes, which marks the control or row under
    /// the cursor and fills the quieter part of a control built from both.
    pub element_hover: Hsla,
    /// The heavier of the two, which marks the control being pressed and fills
    /// the part of a control that stands over the part `element_hover` fills.
    pub element_active: Hsla,
    /// The line marking an edge between two surfaces.
    pub border: Hsla,
    /// The quieter line, a wash marking a divider between parts of one surface.
    pub border_variant: Hsla,
    /// The edge of the control holding keyboard focus.
    pub border_focused: Hsla,
    /// A ruler dropline, or the graph paper a chart is plotted on.
    pub gridline: Hsla,
    pub text: Hsla,
    /// Secondary text: a caption, a unit, a status line.
    pub text_muted: Hsla,
    pub text_disabled: Hsla,
    /// Text carrying the accent, such as a link.
    pub text_accent: Hsla,
    /// Text over an `accent` fill.
    pub text_on_accent: Hsla,
    /// The single hue marking what the user has picked out.
    pub accent: Hsla,
    /// A wash of `accent`, marking a chosen row or a range dragged out.
    pub selection: Hsla,
    /// Draws the eye to what a view is calling out for the moment — a
    /// just-toggled node, a phase boundary, whatever the pointer rests on. It is
    /// read as a rule and as the label naming it, so it holds text contrast
    /// against every surface on either appearance.
    pub highlight: Hsla,
    pub error: Hsla,
    pub warning: Hsla,
    pub success: Hsla,
    pub info: Hsla,
    /// Carries the base opacity a drop shadow paints at, which a caller fading a
    /// shadow in scales down.
    pub shadow: Hsla,
}

/// Hues for coloring data by category — a node's kind, an edge's provenance, a
/// span's workload. Each view maps its own categories onto these slots; the
/// names say only where a slot sits on the wheel, and every pair is separated
/// enough to tell apart at a glance.
#[derive(Clone, Copy)]
pub struct DataColors {
    pub red: Hsla,
    pub orange: Hsla,
    pub yellow: Hsla,
    pub green: Hsla,
    pub cyan: Hsla,
    pub blue: Hsla,
    pub purple: Hsla,
    /// For a category that carries no meaning of its own, such as a catch-all.
    pub neutral: Hsla,
}

impl DataColors {
    pub const CHROMATIC: usize = 7;

    /// The slots that carry a hue, in the wheel order they are declared in, for
    /// a view assigning categories to slots by position rather than by name.
    pub fn chromatic(&self) -> [Hsla; Self::CHROMATIC] {
        [
            self.red,
            self.orange,
            self.yellow,
            self.green,
            self.cyan,
            self.blue,
            self.purple,
        ]
    }
}

/// The ANSI colors a terminal resolves its cell colors against, indexed as the
/// standard 0-7 then bright 8-15.
#[derive(Clone, Copy)]
pub struct TerminalColors {
    pub ansi: [Hsla; Self::ANSI],
}

impl TerminalColors {
    pub const ANSI: usize = 16;
}
