//! Labeled contact sheet for scenario captures: a grid of thumbnails, each under a caption
//! strip (`LABEL - T=123 (2.05S)`), drawn with a built-in 5x7 bitmap font (no font files).

use image::{Rgba, RgbaImage, imageops};

const GLYPH_W: u32 = 5;
const GLYPH_H: u32 = 7;
const BACKGROUND: Rgba<u8> = Rgba([24, 24, 26, 255]);
const CAPTION_BG: Rgba<u8> = Rgba([38, 38, 42, 255]);
const TEXT: Rgba<u8> = Rgba([236, 232, 220, 255]);
const ACCENT: Rgba<u8> = Rgba([255, 191, 64, 255]);

/// One captured frame and its caption.
pub struct SheetFrame {
    pub image: RgbaImage,
    pub caption: String,
}

/// Layout knobs for [`compose_contact_sheet`].
#[derive(Debug, Clone, Copy)]
pub struct SheetLayout {
    pub columns: u32,
    pub thumb_width: u32,
    pub gutter: u32,
    pub text_scale: u32,
}

impl Default for SheetLayout {
    fn default() -> Self {
        Self {
            columns: 3,
            thumb_width: 640,
            gutter: 12,
            text_scale: 3,
        }
    }
}

/// 5x7 glyph rows (low 5 bits, MSB = leftmost pixel). Unknown characters render as `?`.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0; 7],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        ',' => [0, 0, 0, 0, 0x0C, 0x04, 0x08],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        '=' => [0, 0, 0x1F, 0, 0x1F, 0, 0],
        '/' => [0, 0x01, 0x02, 0x04, 0x08, 0x10, 0],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '+' => [0, 0x04, 0x04, 0x1F, 0x04, 0x04, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0x1F],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '$' => [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04],
        _ => [0x0E, 0x11, 0x01, 0x02, 0x04, 0, 0x04],
    }
}

/// Pixel width of `text` at `scale` (one blank column between glyphs).
#[must_use]
pub fn text_width(text: &str, scale: u32) -> u32 {
    let n = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
    n.saturating_mul(GLYPH_W + 1).saturating_mul(scale)
}

/// Draws `text` with its top-left corner at `(x, y)`, clipped to the image.
pub fn draw_text(img: &mut RgbaImage, x: u32, y: u32, text: &str, scale: u32, color: Rgba<u8>) {
    let scale = scale.max(1);
    let mut pen_x = x;
    for c in text.chars() {
        let rows = glyph(c);
        for (row_idx, bits) in rows.iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) == 0 {
                    continue;
                }
                let px = pen_x.saturating_add(col * scale);
                let py = y.saturating_add(u32::try_from(row_idx).unwrap_or(0) * scale);
                for dy in 0..scale {
                    for dx in 0..scale {
                        let (tx, ty) = (px.saturating_add(dx), py.saturating_add(dy));
                        if tx < img.width() && ty < img.height() {
                            img.put_pixel(tx, ty, color);
                        }
                    }
                }
            }
        }
        pen_x = pen_x.saturating_add((GLYPH_W + 1) * scale);
    }
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, color: Rgba<u8>) {
    for ty in y..y.saturating_add(h).min(img.height()) {
        for tx in x..x.saturating_add(w).min(img.width()) {
            img.put_pixel(tx, ty, color);
        }
    }
}

/// Composes `frames` into one labeled grid under a `title` banner. Thumbnails keep the
/// first frame's aspect ratio; an empty frame list yields a title-only sheet.
#[must_use]
pub fn compose_contact_sheet(title: &str, frames: &[SheetFrame], layout: SheetLayout) -> RgbaImage {
    let columns = layout.columns.max(1);
    let thumb_w = layout.thumb_width.max(16);
    let (src_w, src_h) = frames
        .first()
        .map_or((16, 9), |f| (f.image.width().max(1), f.image.height().max(1)));
    let thumb_h = (u64::from(thumb_w) * u64::from(src_h) / u64::from(src_w)).max(1);
    let thumb_h = u32::try_from(thumb_h).unwrap_or(u32::MAX / 4);
    let scale = layout.text_scale.max(1);
    let caption_h = GLYPH_H * scale + 2 * layout.gutter.clamp(6, 16);
    let title_h = GLYPH_H * (scale + 1) + 2 * layout.gutter;
    let count = u32::try_from(frames.len()).unwrap_or(0);
    let rows = count.div_ceil(columns).max(1);
    let cell_w = thumb_w + layout.gutter;
    let cell_h = caption_h + thumb_h + layout.gutter;
    let width = columns * cell_w + layout.gutter;
    let height = title_h + rows * cell_h + layout.gutter;

    let mut sheet = RgbaImage::from_pixel(width, height, BACKGROUND);
    draw_text(&mut sheet, layout.gutter, layout.gutter, title, scale + 1, ACCENT);

    for (idx, frame) in frames.iter().enumerate() {
        let idx = u32::try_from(idx).unwrap_or(0);
        let x = layout.gutter + (idx % columns) * cell_w;
        let y = title_h + (idx / columns) * cell_h;
        fill(&mut sheet, x, y, thumb_w, caption_h, CAPTION_BG);
        let pad = (caption_h - GLYPH_H * scale) / 2;
        let max_chars = (thumb_w.saturating_sub(2 * pad) / ((GLYPH_W + 1) * scale)) as usize;
        let caption: String = frame.caption.chars().take(max_chars).collect();
        draw_text(&mut sheet, x + pad, y + pad, &caption, scale, TEXT);
        let thumb = imageops::resize(&frame.image, thumb_w, thumb_h, imageops::FilterType::Triangle);
        imageops::overlay(&mut sheet, &thumb, i64::from(x), i64::from(y + caption_h));
    }
    sheet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_grid_dimensions_and_captions() {
        let frame = |shade: u8| SheetFrame {
            image: RgbaImage::from_pixel(320, 180, Rgba([shade, shade, shade, 255])),
            caption: "INTACT - T=0 (0.00S)".to_string(),
        };
        let layout = SheetLayout {
            columns: 2,
            thumb_width: 160,
            gutter: 8,
            text_scale: 2,
        };
        let sheet = compose_contact_sheet("RANCH", &[frame(10), frame(200), frame(90)], layout);
        // 2 columns of (160 + 8) + 8, 2 rows.
        assert_eq!(sheet.width(), 2 * 168 + 8);
        assert!(sheet.height() > 2 * 90);
        // Thumbnail pixels carry their source shade; captions put text pixels on the strip.
        let caption_h = 7 * 2 + 2 * 8; // gutter 8 is inside the 6..=16 caption padding clamp
        let title_h = 7 * 3 + 2 * 8;
        let thumb_px = sheet.get_pixel(8 + 80, title_h + caption_h + 45);
        assert_eq!(thumb_px.0[0], 10);
        let strip_has_text = (0..160).any(|dx| *sheet.get_pixel(8 + dx, title_h + 8 + 3) == TEXT);
        assert!(strip_has_text);
    }

    #[test]
    fn empty_and_unknown_inputs_do_not_panic() {
        let sheet = compose_contact_sheet("NO FRAMES ~ ü", &[], SheetLayout::default());
        assert!(sheet.width() > 0 && sheet.height() > 0);
        let mut tiny = RgbaImage::new(3, 3);
        draw_text(&mut tiny, 2, 2, "WIDE TEXT CLIPPED", 4, TEXT);
        assert_eq!(text_width("AB", 2), 24);
    }
}
