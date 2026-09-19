//! The image the window system draws where the pointer carries a panel, once a
//! drag has left the window it started in and the window system has taken the
//! gesture over.

use gpui::{
    DevicePixels, DragIcon, Hsla, IconScale, Pixels, Point, Rgba, ShapedLine, SharedString, Size,
    Window, point, size,
};

/// How many bytes of the image one pixel takes.
const BYTES_PER_PIXEL: usize = DragIcon::BYTES_PER_PIXEL;

/// The chip a dragged panel is drawn as: its name on a filled, rounded plate
/// edged in the accent colour.
pub(crate) struct Chip {
    pub title: SharedString,
    pub font_size: Pixels,
    pub height: Pixels,
    /// The widest the chip is drawn, beyond which the name is cut, so that one
    /// long title cannot cover what the drag is aimed at.
    pub max_width: Pixels,
    /// How much room the name is given either side of it.
    pub padding: Pixels,
    pub radius: Pixels,
    pub edge: Pixels,
    pub fill: Hsla,
    pub edge_color: Hsla,
    pub ink: Hsla,
    /// How many image pixels go to one of the window's, so that the chip is
    /// drawn at the size it was measured at on a scaled display.
    pub scale: IconScale,
}

/// `chip` rasterized for the window system to draw.
pub(crate) fn rasterize(chip: &Chip, window: &mut Window) -> DragIcon {
    let font = gpui::font(bviz_ui_assets::UI);
    let line =
        bviz_ui_assets::shape_line(window, chip.title.clone(), font, chip.font_size, chip.ink);
    let width = (line.width + chip.padding * 2.0).min(chip.max_width);
    DragIcon::new(size(width, chip.height), chip.scale, |pixels, extent| {
        draw_plate(pixels, extent, chip);
        let height = height(extent, chip.scale);
        let baseline = point(
            chip.padding,
            (height - line.ascent - line.descent) * 0.5 + line.ascent,
        );
        draw_line(pixels, extent, &line, baseline, chip, window);
    })
}

/// Fills the plate the name is set on, across the whole of the image: the edge
/// over all of it, and the fill over everything the edge does not take, so that
/// the two meet without a seam.
fn draw_plate(pixels: &mut [u8], extent: Size<DevicePixels>, chip: &Chip) {
    let edge_color = Rgba::from(chip.edge_color);
    let fill = Rgba::from(chip.fill);
    let scale = chip.scale.get() as f32;
    let (width, height) = (extent.width.0 as f32, extent.height.0 as f32);
    let radius = f32::from(chip.radius) * scale;
    let edge = f32::from(chip.edge) * scale;
    for y in 0..extent.height.0 {
        for x in 0..extent.width.0 {
            let at = index(extent, x, y);
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let outer = covered(px, py, width, height, radius);
            over(&mut pixels[at..], edge_color, outer);
            let inner = covered(
                px - edge,
                py - edge,
                width - edge * 2.0,
                height - edge * 2.0,
                (radius - edge).max(0.0),
            );
            over(&mut pixels[at..], fill, inner);
        }
    }
}

/// Draws the name from `baseline`, clipped to the room the plate leaves it, so
/// that a title too long for the chip is cut where the chip ends.
fn draw_line(
    pixels: &mut [u8],
    extent: Size<DevicePixels>,
    line: &ShapedLine,
    baseline: Point<Pixels>,
    chip: &Chip,
    window: &Window,
) {
    let scale = chip.scale.get() as f32;
    let ink = Rgba::from(chip.ink);
    let cut = extent.width.0 - device(chip.padding, scale).0;
    for run in &line.runs {
        for glyph in &run.glyphs {
            let params = gpui::RenderGlyphParams {
                font_id: run.font_id,
                glyph_id: glyph.id,
                font_size: line.font_size,
                subpixel_variant: point(0, 0),
                scale_factor: scale,
                is_emoji: glyph.is_emoji,
                subpixel_rendering: false,
                dilation: 0,
            };
            let text_system = window.text_system();
            let Ok(bounds) = text_system.raster_bounds(&params) else {
                continue;
            };
            let Ok((glyph_extent, mask)) = text_system.rasterize_glyph(&params) else {
                continue;
            };
            let origin = point(
                device(baseline.x + glyph.position.x, scale) + bounds.origin.x,
                device(baseline.y + glyph.position.y, scale) + bounds.origin.y,
            );
            draw_glyph(pixels, extent, origin, glyph_extent, &mask, ink, cut);
        }
    }
}

/// Lays one rasterized glyph over the image, `mask` holding one byte of
/// coverage per pixel where the glyph is drawn in the ink it was asked for and
/// four bytes of its own colour per pixel where it carries one. Nothing past
/// `cut` along the row, nor outside the image, is drawn.
fn draw_glyph(
    pixels: &mut [u8],
    extent: Size<DevicePixels>,
    origin: Point<DevicePixels>,
    glyph: Size<DevicePixels>,
    mask: &[u8],
    ink: Rgba,
    cut: i32,
) {
    let colored = mask.len() == (glyph.width.0 * glyph.height.0) as usize * BYTES_PER_PIXEL;
    for row in 0..glyph.height.0 {
        let y = origin.y.0 + row;
        if y < 0 || y >= extent.height.0 {
            continue;
        }
        for column in 0..glyph.width.0 {
            let x = origin.x.0 + column;
            if x < 0 || x >= extent.width.0.min(cut) {
                continue;
            }
            let read = (row * glyph.width.0 + column) as usize;
            let at = index(extent, x, y);
            match colored {
                true => {
                    let from = read * BYTES_PER_PIXEL;
                    let Some(pixel) = mask.get(from..from + BYTES_PER_PIXEL) else {
                        continue;
                    };
                    let alpha = f32::from(pixel[3]) / 255.0;
                    let color = Rgba {
                        b: f32::from(pixel[0]) / 255.0,
                        g: f32::from(pixel[1]) / 255.0,
                        r: f32::from(pixel[2]) / 255.0,
                        a: alpha,
                    };
                    over(&mut pixels[at..], color, 1.0);
                }
                false => {
                    let Some(&coverage) = mask.get(read) else {
                        continue;
                    };
                    over(&mut pixels[at..], ink, f32::from(coverage) / 255.0);
                }
            }
        }
    }
}

/// Lays `color` over the pixel `image` starts at, `coverage` of the pixel
/// covered by it. Every pixel holds blue, green, red and an alpha the other
/// three are already multiplied by, so this is a plain source-over.
fn over(image: &mut [u8], color: Rgba, coverage: f32) {
    let alpha = color.a * coverage;
    if alpha <= 0.0 {
        return;
    }
    let source = [color.b, color.g, color.r, 1.0];
    for (channel, value) in image[..BYTES_PER_PIXEL].iter_mut().zip(source) {
        let under = f32::from(*channel) / 255.0 * (1.0 - alpha);
        *channel = ((under + value * alpha) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

/// Where the pixel at (`x`, `y`) of an image `extent` across starts.
fn index(extent: Size<DevicePixels>, x: i32, y: i32) -> usize {
    (y * extent.width.0 + x) as usize * BYTES_PER_PIXEL
}

/// How much of the pixel centred on (`x`, `y`) a rectangle `width` by `height`
/// from the origin with corners of `radius` covers, read as the distance from
/// its boundary across the one pixel that boundary passes through.
fn covered(x: f32, y: f32, width: f32, height: f32, radius: f32) -> f32 {
    if width <= 0.0 || height <= 0.0 {
        return 0.0;
    }
    let across = (x - width * 0.5).abs() - (width * 0.5 - radius);
    let down = (y - height * 0.5).abs() - (height * 0.5 - radius);
    let outside = across.max(0.0).hypot(down.max(0.0));
    let distance = outside + across.max(down).min(0.0) - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// `extent` of the window's pixels in the image's, which are `scale` to one.
fn device(extent: Pixels, scale: f32) -> DevicePixels {
    DevicePixels((f32::from(extent) * scale).round() as i32)
}

/// How tall the image is in the window's own pixels, which the image rounds out
/// to a whole number of and which the name is centred down.
fn height(extent: Size<DevicePixels>, scale: IconScale) -> Pixels {
    gpui::px((extent.height.0 / scale.get()) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pixel well inside a rounded rectangle is wholly covered, one well
    /// outside it not at all.
    #[test]
    fn a_rounded_rectangle_covers_its_middle_and_not_its_surroundings() {
        assert_eq!(covered(20.0, 10.0, 40.0, 20.0, 6.0), 1.0);
        assert_eq!(covered(-4.0, 10.0, 40.0, 20.0, 6.0), 0.0);
        assert_eq!(covered(20.0, 30.0, 40.0, 20.0, 6.0), 0.0);
    }

    /// The corners a radius rounds off are cut away, while the edges between
    /// them stay covered.
    #[test]
    fn a_radius_rounds_the_corners_off() {
        assert_eq!(covered(0.5, 0.5, 40.0, 20.0, 6.0), 0.0);
        assert_eq!(covered(20.0, 0.5, 40.0, 20.0, 6.0), 1.0);
        assert_eq!(covered(0.5, 10.0, 40.0, 20.0, 6.0), 1.0);
    }

    /// Laying an opaque colour over a pixel leaves that colour, premultiplied.
    #[test]
    fn an_opaque_colour_takes_the_pixel() {
        let mut image = [0u8; BYTES_PER_PIXEL];
        let red = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        over(&mut image, red, 1.0);
        assert_eq!(image, [0, 0, 255, 255]);
    }

    /// Half the coverage of an opaque colour over an empty pixel leaves half of
    /// it, alpha included.
    #[test]
    fn half_coverage_leaves_half_the_colour() {
        let mut image = [0u8; BYTES_PER_PIXEL];
        let white = Rgba {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        };
        over(&mut image, white, 0.5);
        assert_eq!(image, [128, 128, 128, 128]);
    }

    /// A Wayland compositor refuses a buffer whose dimensions the scale it is
    /// drawn at does not divide, and disconnects the client that commits one,
    /// which takes every window of the application down mid-gesture. Whatever a
    /// shaped title measures, and whatever the display is scaled by, both
    /// dimensions divide.
    #[test]
    fn an_icon_measures_a_whole_number_of_window_pixels() {
        // The widths shaped titles land on: whole, a hair either side of whole,
        // and the halves and thirds that a rounded measure sits exactly on.
        let widths = [
            1.0, 1.5, 12.0, 12.5, 26.4, 26.5, 26.6, 63.25, 100.0, 113.7, 219.999, 220.0,
        ];
        for scale in 1..=4 {
            let scale = IconScale::for_display(scale as f32);
            for width in widths {
                let icon = DragIcon::new(
                    size(gpui::px(width), gpui::px(26.0)),
                    scale,
                    |pixels, extent| {
                        assert_eq!(
                            pixels.len(),
                            extent.width.0 as usize * extent.height.0 as usize * BYTES_PER_PIXEL
                        );
                    },
                );
                let measured = icon.size();
                assert_eq!(
                    (
                        measured.width.0 % scale.get(),
                        measured.height.0 % scale.get()
                    ),
                    (0, 0),
                    "{width} at {scale:?} measures {measured:?}"
                );
                assert!(measured.width.0 >= scale.get());
            }
        }
    }

    /// A display scaled by a fraction draws the icon at the whole scale above
    /// it, so the image carries at least as many pixels as the display asks
    /// for; an unscaled display, and one reporting no scale at all, draw it at
    /// one image pixel to the window's.
    #[test]
    fn a_fractionally_scaled_display_draws_at_the_whole_scale_above_it() {
        assert_eq!(IconScale::for_display(1.0).get(), 1);
        assert_eq!(IconScale::for_display(1.25).get(), 2);
        assert_eq!(IconScale::for_display(1.5).get(), 2);
        assert_eq!(IconScale::for_display(2.0).get(), 2);
        assert_eq!(IconScale::for_display(2.75).get(), 3);
        assert_eq!(IconScale::for_display(0.0).get(), 1);
    }
}
