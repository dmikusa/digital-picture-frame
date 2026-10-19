// Photo Frame Manager — DRM/GBM/EGL digital photo frame.
// Copyright (C) 2026 Daniel Mikusa <dan@mikusa.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::config::Config;
use image::{DynamicImage, ImageBuffer, Rgb, RgbImage};
use qrcode::QrCode;
use rand::{distributions::Alphanumeric, Rng};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Config screen layout
// ---------------------------------------------------------------------------

/// Fraction of the screen height left blank above the QR code.
const QR_TOP_MARGIN: f32 = 0.10;
/// Fraction of the screen height used as the maximum QR code size.
const QR_HEIGHT_RATIO: u32 = 50;
/// Fraction of the screen width used as the maximum QR code size.
const QR_WIDTH_RATIO: u32 = 80;
/// Gap between the bottom of the QR code and the top of the first text line.
const QR_TEXT_GAP: f32 = 30.0;
/// Distance from the bottom edge of the screen to the footer baseline.
const FOOTER_BASELINE_MARGIN: f32 = 40.0;
/// Instruction font size, as a fraction of the screen height.
const TEXT_SIZE_RATIO: f32 = 0.04;
/// Footer font size, as a fraction of the screen height.
const FOOTER_SIZE_RATIO: f32 = 0.03;
/// Fallback file used by [`render_config_screen`].
const DEFAULT_CONFIG_SCREEN_PATH: &str = "/tmp/photo-frame-qr.jpg";

/// Computed positions for every element on the config screen.
///
/// The renderer and the layout tests both use this so that what the tests
/// assert about the layout is exactly what gets drawn.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConfigScreenLayout {
    pub qr_x: u32,
    pub qr_y: u32,
    /// Width/height of the drawn QR code in pixels.
    #[allow(dead_code)] // exercised by the layout tests
    pub qr_pixels: u32,
    /// Pixels per QR module.
    pub module_px: u32,
    pub font_size: f32,
    pub footer_font_size: f32,
    /// Baseline of each instruction line, top to bottom.
    pub baselines: [f32; 3],
    pub footer_baseline: f32,
    /// Distance between instruction line baselines.
    #[allow(dead_code)] // exercised by the layout tests
    pub line_height: f32,
}

impl ConfigScreenLayout {
    #[allow(dead_code)] // exercised by the layout tests
    pub(crate) fn qr_bottom(&self) -> u32 {
        self.qr_y + self.qr_pixels
    }
}

fn compute_layout(
    width: u32,
    height: u32,
    module_count: u32,
    font: &fontdue::Font,
) -> ConfigScreenLayout {
    let qr_area = height * QR_HEIGHT_RATIO / 100;
    let qr_size = qr_area.min(width * QR_WIDTH_RATIO / 100).max(32);
    let module_px = (qr_size / module_count).max(1);
    let qr_pixels = module_count * module_px;
    let qr_x = width.saturating_sub(qr_pixels) / 2;
    let qr_y = (height as f32 * QR_TOP_MARGIN) as u32;

    let font_size = (height as f32 * TEXT_SIZE_RATIO).max(12.0);
    let footer_font_size = (height as f32 * FOOTER_SIZE_RATIO).max(12.0);
    let line_metrics = font
        .horizontal_line_metrics(font_size)
        .unwrap_or(fontdue::LineMetrics {
            ascent: font_size * 0.7,
            descent: -font_size * 0.3,
            line_gap: 0.0,
            new_line_size: font_size * 1.2,
        });
    let line_height = line_metrics.new_line_size;
    // Put the top of the first line's em box QR_TEXT_GAP below the QR code,
    // then move down by the ascent to land on the baseline.
    let first_baseline = (qr_y + qr_pixels) as f32 + QR_TEXT_GAP + line_metrics.ascent;

    ConfigScreenLayout {
        qr_x,
        qr_y,
        qr_pixels,
        module_px,
        font_size,
        footer_font_size,
        baselines: [
            first_baseline,
            first_baseline + line_height,
            first_baseline + line_height * 2.0,
        ],
        footer_baseline: height as f32 - FOOTER_BASELINE_MARGIN,
        line_height,
    }
}

// ---------------------------------------------------------------------------
// Password generation
// ---------------------------------------------------------------------------

/// Generate a random 8-character alphanumeric password.
pub fn generate_password() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect()
}

// ---------------------------------------------------------------------------
// Font / text rendering (fontdue)
// ---------------------------------------------------------------------------

fn load_font() -> Result<fontdue::Font, String> {
    let data: &[u8] = include_bytes!("../fonts/DejaVuSansMono.ttf");
    fontdue::Font::from_bytes(data, fontdue::FontSettings::default())
        .map_err(|e| format!("Failed to parse font: {e}"))
}

fn measure_text(text: &str, font_size: f32, font: &fontdue::Font) -> f32 {
    text.chars()
        .map(|ch| {
            let (metrics, _) = font.rasterize(ch, font_size);
            metrics.advance_width
        })
        .sum()
}

fn draw_centered(
    img: &mut RgbImage,
    baseline: f32,
    text: &str,
    color: Rgb<u8>,
    font_size: f32,
    font: &fontdue::Font,
) {
    let width = measure_text(text, font_size, font);
    let x = ((img.width() as f32 - width) / 2.0).max(0.0);
    draw_text(img, x, baseline, text, color, font_size, font);
}

fn draw_text(
    img: &mut RgbImage,
    x: f32,
    baseline: f32,
    text: &str,
    color: Rgb<u8>,
    font_size: f32,
    font: &fontdue::Font,
) {
    let mut cx = x;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, font_size);
        // fontdue's `ymin` is the offset of the bitmap's bottom edge from the
        // baseline, positive upwards (negative for descenders).  The bitmap's
        // top edge is therefore `ymin + height` above the baseline.
        let glyph_y = (baseline - metrics.ymin as f32 - metrics.height as f32).round() as i32;
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha > 0 {
                    let px = (cx.round() as i32 + metrics.xmin + col as i32).max(0) as u32;
                    let py = glyph_y + row as i32;
                    if px < img.width() && py >= 0 && (py as u32) < img.height() {
                        let existing = img.get_pixel(px, py as u32);
                        let blend = alpha as f32 / 255.0;
                        let r = (existing[0] as f32
                            + (color[0] as f32 - existing[0] as f32) * blend)
                            as u8;
                        let g = (existing[1] as f32
                            + (color[1] as f32 - existing[1] as f32) * blend)
                            as u8;
                        let b = (existing[2] as f32
                            + (color[2] as f32 - existing[2] as f32) * blend)
                            as u8;
                        img.put_pixel(px, py as u32, Rgb([r, g, b]));
                    }
                }
            }
        }
        cx += metrics.advance_width;
    }
}

// ---------------------------------------------------------------------------
// QR generation
// ---------------------------------------------------------------------------

pub fn generate_qr_jpeg(url: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let code = QrCode::new(url).map_err(|e| format!("QR encode error: {e}"))?;
    let module_count = code.width() as u32;
    let qr_size = (width.min(height) * 8 / 10).max(32);
    let module_px = qr_size / module_count;

    let mut img: RgbImage = ImageBuffer::new(width, height);
    for pixel in img.pixels_mut() {
        *pixel = Rgb([0u8, 0, 0]);
    }

    let qr_x = (width.saturating_sub(module_count * module_px)) / 2;
    let qr_y = (height.saturating_sub(module_count * module_px)) / 2;

    for y in 0..module_count {
        for x in 0..module_count {
            if code[(x as usize, y as usize)] == qrcode::Color::Dark {
                let px_start = qr_x + x * module_px;
                let py_start = qr_y + y * module_px;
                for dy in 0..module_px {
                    for dx in 0..module_px {
                        let px = px_start + dx;
                        let py = py_start + dy;
                        if px < width && py < height {
                            img.put_pixel(px, py, Rgb([255u8, 255, 255]));
                        }
                    }
                }
            }
        }
    }

    let mut jpeg_bytes: Vec<u8> = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg_bytes),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| format!("JPEG encode error: {e}"))?;
    Ok(jpeg_bytes)
}

// ---------------------------------------------------------------------------
// Full config-mode screen
// ---------------------------------------------------------------------------

pub fn render_config_screen(
    config: &Config,
    admin_url: &str,
    password: &str,
    fallback_ip: &str,
) -> Result<PathBuf, String> {
    render_config_screen_to(
        config,
        admin_url,
        password,
        fallback_ip,
        Path::new(DEFAULT_CONFIG_SCREEN_PATH),
    )
}

/// Render the config screen to a specific path.
///
/// Split out from [`render_config_screen`] so tests can each write to their
/// own file instead of racing over the shared `/tmp` path.
fn render_config_screen_to(
    config: &Config,
    admin_url: &str,
    password: &str,
    fallback_ip: &str,
    out_path: &Path,
) -> Result<PathBuf, String> {
    let (width, height) = config.resolution();
    let font = load_font()?;
    let white = Rgb([255u8, 255, 255]);

    let qr_url = format!("http://photo-frame:{}@{admin_url}", password);
    let code = QrCode::new(&qr_url).map_err(|e| format!("QR encode error: {e}"))?;
    let module_count = code.width() as u32;
    let layout = compute_layout(width, height, module_count, &font);

    let mut img: RgbImage = ImageBuffer::new(width, height);
    for pixel in img.pixels_mut() {
        *pixel = Rgb([0u8, 0, 0]);
    }

    // Draw QR
    for y in 0..module_count {
        for x in 0..module_count {
            if code[(x as usize, y as usize)] == qrcode::Color::Dark {
                let x0 = layout.qr_x + x * layout.module_px;
                let y0 = layout.qr_y + y * layout.module_px;
                for dy in 0..layout.module_px {
                    for dx in 0..layout.module_px {
                        let px = x0 + dx;
                        let py = y0 + dy;
                        if px < width && py < height {
                            img.put_pixel(px, py, white);
                        }
                    }
                }
            }
        }
    }

    let font_size = layout.font_size;
    let url_line = format!("http://{admin_url}");
    draw_centered(
        &mut img,
        layout.baselines[0],
        &url_line,
        white,
        font_size,
        &font,
    );

    let pw_line = format!("Password: {password}");
    draw_centered(
        &mut img,
        layout.baselines[1],
        &pw_line,
        white,
        font_size,
        &font,
    );

    let ip_line = format!(
        "Or: http://{fallback_ip}:{port}",
        port = admin_url
            .split(':')
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or("8080")
    );
    draw_centered(
        &mut img,
        layout.baselines[2],
        &ip_line,
        white,
        font_size,
        &font,
    );

    draw_centered(
        &mut img,
        layout.footer_baseline,
        "Insert USB to configure (remove to cancel)",
        white,
        layout.footer_font_size,
        &font,
    );

    let out_path = PathBuf::from(out_path);
    DynamicImage::ImageRgb8(img)
        .save_with_format(&out_path, image::ImageFormat::Jpeg)
        .map_err(|e| format!("Failed to save config screen: {e}"))?;

    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN_URL: &str = "photo-frame.local:8147";
    const PASSWORD: &str = "abc12345";
    const FALLBACK_IP: &str = "192.168.1.5";
    /// Pixels brighter than this count as ink.  Fully lit text/QR pixels are
    /// 255, while JPEG ringing around the edges stays well below this.
    const INK_THRESHOLD: u8 = 200;

    fn test_config(width: u32, height: u32) -> Config {
        Config {
            photos_dir: PathBuf::from("/tmp/photos"),
            display_socket_path: PathBuf::from("/tmp/sock"),
            control_socket_path: PathBuf::from("/tmp/control.sock"),
            native_resolution: format!("{width}x{height}"),
            aspect_ratio_mode: crate::config::AspectRatioMode::Fit,
            batch_delete_size: 20,
            log_max_size: 262144,
            log_max_files: 2,
            remote_sources: vec![],
        }
    }

    /// Render the screen at the given size and return the decoded image plus
    /// the layout that was used to draw it.
    fn render_for_test(width: u32, height: u32, dir: &Path) -> (RgbImage, ConfigScreenLayout) {
        let config = test_config(width, height);
        let font = load_font().unwrap();
        let qr_url = format!("http://photo-frame:{PASSWORD}@{ADMIN_URL}");
        let code = QrCode::new(&qr_url).unwrap();
        let layout = compute_layout(width, height, code.width() as u32, &font);

        let path = dir.join(format!("config-{width}x{height}.jpg"));
        let path =
            render_config_screen_to(&config, ADMIN_URL, PASSWORD, FALLBACK_IP, &path).unwrap();
        (image::open(path).unwrap().to_rgb8(), layout)
    }

    /// Contiguous runs of rows that contain at least one ink pixel.
    fn row_bands(img: &RgbImage) -> Vec<(u32, u32)> {
        let mut bands = Vec::new();
        let mut start = None;
        for y in 0..img.height() {
            let inked = (0..img.width()).any(|x| img.get_pixel(x, y)[0] > INK_THRESHOLD);
            match (inked, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    bands.push((s, y - 1));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            bands.push((s, img.height() - 1));
        }
        bands
    }

    /// Left-most and right-most ink columns within a row range.
    fn column_extent(img: &RgbImage, top: u32, bottom: u32) -> (u32, u32) {
        let mut left = img.width();
        let mut right = 0;
        for y in top..=bottom {
            for x in 0..img.width() {
                if img.get_pixel(x, y)[0] > INK_THRESHOLD {
                    left = left.min(x);
                    right = right.max(x);
                }
            }
        }
        assert!(left <= right, "no ink found in rows {top}..={bottom}");
        (left, right)
    }

    /// Bottom-most ink row within a column range, if any.
    fn bottom_in_columns(img: &RgbImage, left: u32, right: u32) -> Option<u32> {
        let mut bottom = None;
        for x in left..right.min(img.width()) {
            for y in 0..img.height() {
                if img.get_pixel(x, y)[0] > INK_THRESHOLD {
                    bottom = Some(bottom.map_or(y, |b: u32| b.max(y)));
                }
            }
        }
        bottom
    }

    /// The row range a piece of text should cover when drawn at `baseline`.
    fn expected_ink_rows(
        text: &str,
        baseline: f32,
        font_size: f32,
        font: &fontdue::Font,
    ) -> (u32, u32) {
        let baseline = baseline.round() as i32;
        let mut top = i32::MAX;
        let mut bottom = i32::MIN;
        for ch in text.chars() {
            let (m, _) = font.rasterize(ch, font_size);
            if m.width == 0 || m.height == 0 {
                continue;
            }
            let glyph_top = baseline - m.ymin - m.height as i32;
            top = top.min(glyph_top);
            bottom = bottom.max(glyph_top + m.height as i32 - 1);
        }
        (top as u32, bottom as u32)
    }

    /// Render `text` and return the bottom-most ink row of each visible glyph.
    fn glyph_bottoms(
        text: &str,
        start_x: f32,
        baseline: f32,
        font_size: f32,
        font: &fontdue::Font,
    ) -> Vec<(char, u32)> {
        let mut img = RgbImage::new(400, 100);
        let white = Rgb([255u8, 255, 255]);
        draw_text(&mut img, start_x, baseline, text, white, font_size, font);

        let mut x = start_x;
        let mut bottoms = Vec::new();
        for ch in text.chars() {
            let (m, _) = font.rasterize(ch, font_size);
            if m.width > 0 && m.height > 0 {
                let left = (x.round() as i32 + m.xmin).max(0) as u32;
                let right = left + m.width as u32;
                let bottom = bottom_in_columns(&img, left, right)
                    .unwrap_or_else(|| panic!("{ch} was not rendered"));
                bottoms.push((ch, bottom));
            }
            x += m.advance_width;
        }
        bottoms
    }

    #[test]
    fn test_generate_password() {
        let pw = generate_password();
        assert_eq!(pw.len(), 8);
        assert!(pw.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn test_generate_qr_jpeg() {
        let jpeg = generate_qr_jpeg("http://example.com", 800, 600).unwrap();
        assert!(!jpeg.is_empty());
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn test_render_config_screen() {
        let config = test_config(800, 600);
        let path = render_config_screen(&config, ADMIN_URL, PASSWORD, FALLBACK_IP).unwrap();
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn test_font_metrics() {
        let font = load_font().unwrap();
        let size = 24.0;

        let (hm, _) = font.rasterize('H', size);
        let (pm, _) = font.rasterize('p', size);

        // fontdue reports `ymin` as the offset of the bitmap's bottom edge
        // from the baseline, positive upwards.
        assert_eq!(hm.ymin, 0, "H should sit on the baseline");
        assert!(pm.ymin < 0, "p should have a negative ymin (descender)");
        assert!(hm.width > 0 && hm.height > 0);
        assert!(pm.width > 0 && pm.height > 0);

        // Line metrics should give sane, non-overlapping spacing.
        let hl = font.horizontal_line_metrics(size).unwrap();
        assert!(
            hl.new_line_size > size * 0.8,
            "line height should be close to font size"
        );
        assert!(
            hl.new_line_size < size * 2.0,
            "line height should not be double font size"
        );
        assert!(hl.ascent > 0.0, "ascent should be positive");
        assert!(hl.descent <= 0.0, "descent should be at or below baseline");
    }

    #[test]
    fn test_glyphs_share_a_baseline() {
        let font = load_font().unwrap();
        let size = 24.0;
        let baseline = 40.0;

        // Spaces isolate each glyph so their ink cannot bleed into a
        // neighbour's column range.  These are the letters and digits that
        // previously looked "a couple of pixels too high".
        let bottoms = glyph_bottoms("H o x e s U 5 9 a", 10.0, baseline, size, &font);
        assert_eq!(bottoms.len(), 9);

        let expected = baseline.round() as i32 - 1;
        for (ch, bottom) in bottoms {
            assert!(
                (bottom as i32 - expected).abs() <= 1,
                "'{ch}' bottoms out at row {bottom}, expected ~{expected} (baseline {baseline})"
            );
        }
    }

    #[test]
    fn test_descenders_extend_below_baseline() {
        let font = load_font().unwrap();
        let size = 24.0;
        let baseline = 40.0;

        let bottoms = glyph_bottoms("H p g", 10.0, baseline, size, &font);
        let bottom_of = |target: char| {
            bottoms
                .iter()
                .find(|(ch, _)| *ch == target)
                .map(|(_, bottom)| *bottom)
                .unwrap_or_else(|| panic!("{target} was not rendered"))
        };

        let h_bottom = bottom_of('H');
        let p_bottom = bottom_of('p');
        let g_bottom = bottom_of('g');

        assert!(h_bottom < baseline as u32, "H should stop at the baseline");
        assert!(
            p_bottom >= baseline as u32 + 3 && p_bottom <= baseline as u32 + 6,
            "p descender bottom {p_bottom} should be 3-6px below baseline {baseline}"
        );
        assert!(
            g_bottom >= baseline as u32 + 3 && g_bottom <= baseline as u32 + 7,
            "g descender bottom {g_bottom} should be 3-7px below baseline {baseline}"
        );
    }

    #[test]
    fn test_config_screen_layout() {
        let dir = tempfile::tempdir().unwrap();
        let font = load_font().unwrap();

        for (width, height) in [(800u32, 600u32), (1024, 768), (1920, 1080)] {
            let (img, layout) = render_for_test(width, height, dir.path());
            let dimensions = (width, height);
            assert_eq!(img.dimensions(), dimensions);

            // ---- Layout numbers -------------------------------------------
            assert_eq!(
                layout.qr_y,
                (height as f32 * QR_TOP_MARGIN) as u32,
                "QR top margin"
            );
            assert_eq!(
                layout.footer_baseline,
                height as f32 - FOOTER_BASELINE_MARGIN,
                "footer baseline"
            );

            let line_metrics = font.horizontal_line_metrics(layout.font_size).unwrap();
            assert!(
                (layout.line_height - line_metrics.new_line_size).abs() < 0.01,
                "line height should come from the font metrics"
            );
            assert!(
                layout.baselines[0] >= layout.qr_bottom() as f32 + QR_TEXT_GAP,
                "first baseline should be below the QR gap"
            );
            assert!(
                layout.baselines[0]
                    <= layout.qr_bottom() as f32 + QR_TEXT_GAP + line_metrics.ascent,
                "first baseline should not drift away from the QR code"
            );
            assert!(
                layout.footer_baseline > layout.baselines[2] + 2.0 * layout.line_height,
                "footer should be clearly separated from the instructions"
            );
            assert!(
                layout.footer_baseline
                    + font
                        .horizontal_line_metrics(layout.footer_font_size)
                        .unwrap()
                        .descent
                    > 0.0,
                "footer should be inside the screen"
            );

            // ---- Rendered pixels -----------------------------------------
            let bands = row_bands(&img);
            assert_eq!(
                bands.len(),
                5,
                "{width}x{height}: expected QR + 3 instruction lines + footer, got {bands:?}"
            );

            let (qr_top, qr_bottom) = bands[0];
            let text_bands = &bands[1..];

            // QR code sits at the top, is horizontally centered, and is not
            // clipped.
            assert!(
                (qr_top as i32 - layout.qr_y as i32).abs() <= 1,
                "{width}x{height}: QR should start at {}, found {qr_top}",
                layout.qr_y
            );
            assert!(
                (qr_bottom as i32 - layout.qr_bottom() as i32).abs() <= 1,
                "{width}x{height}: QR should end at {}, found {qr_bottom}",
                layout.qr_bottom()
            );
            let (qr_left, qr_right) = column_extent(&img, qr_top, qr_bottom);
            assert!(
                ((qr_left + qr_right) as i32 - width as i32).abs() <= 4,
                "{width}x{height}: QR should be centered ({qr_left}..{qr_right})"
            );
            assert!(
                qr_left >= 5 && qr_right < width - 5,
                "{width}x{height}: QR should not touch the screen edges"
            );

            // There is visible breathing room between the QR code and the
            // first line of text.
            assert!(
                text_bands[0].0 as i32 - qr_bottom as i32 >= 20,
                "{width}x{height}: only {}px between QR and text",
                text_bands[0].0 as i32 - qr_bottom as i32
            );

            // Instruction lines are evenly spaced.
            for i in 0..2 {
                let gap = text_bands[i + 1].0 as f32 - text_bands[i].0 as f32;
                assert!(
                    (gap - layout.line_height).abs() <= 2.0,
                    "{width}x{height}: line {} to {} gap {gap:.1} should be ~{}",
                    i + 1,
                    i + 2,
                    layout.line_height
                );
            }

            // Every line's ink lands on the baseline the layout asked for.
            let lines = [
                format!("http://{ADMIN_URL}"),
                format!("Password: {PASSWORD}"),
                format!("Or: http://{FALLBACK_IP}:8147"),
                "Insert USB to configure (remove to cancel)".to_string(),
            ];
            for i in 0..3 {
                let (expected_top, expected_bottom) =
                    expected_ink_rows(&lines[i], layout.baselines[i], layout.font_size, &font);
                assert!(
                    (text_bands[i].0 as i32 - expected_top as i32).abs() <= 2,
                    "{width}x{height}: line {i} starts at {}, expected {expected_top}",
                    text_bands[i].0
                );
                assert!(
                    (text_bands[i].1 as i32 - expected_bottom as i32).abs() <= 2,
                    "{width}x{height}: line {i} ends at {}, expected {expected_bottom}",
                    text_bands[i].1
                );
            }
            let (footer_top, footer_bottom) = expected_ink_rows(
                &lines[3],
                layout.footer_baseline,
                layout.footer_font_size,
                &font,
            );
            assert!(
                (text_bands[3].0 as i32 - footer_top as i32).abs() <= 2,
                "{width}x{height}: footer starts at {}, expected {footer_top}",
                text_bands[3].0
            );
            assert!(
                (text_bands[3].1 as i32 - footer_bottom as i32).abs() <= 2,
                "{width}x{height}: footer ends at {}, expected {footer_bottom}",
                text_bands[3].1
            );

            // Text is centered and has margins on both sides.  Side bearings
            // differ per glyph, so allow a small fraction of the width.
            let center_tolerance = ((width as f32 * 0.005) as i32).max(4);
            for (i, band) in text_bands.iter().enumerate() {
                let (left, right) = column_extent(&img, band.0, band.1);
                assert!(
                    ((left + right) as i32 - width as i32).abs() <= center_tolerance,
                    "{width}x{height}: text line {i} should be centered ({left}..{right})"
                );
                assert!(
                    left >= 10 && right < width - 10,
                    "{width}x{height}: text line {i} should not touch the screen edges"
                );
            }

            // The footer is inside the screen with a real margin below it.
            let bottom_margin = height as i32 - 1 - text_bands[3].1 as i32;
            assert!(
                bottom_margin >= 20,
                "{width}x{height}: footer only {bottom_margin}px from the bottom"
            );
            assert!(
                text_bands[3].0 as i32 - text_bands[2].1 as i32 >= 20,
                "{width}x{height}: footer is too close to the instructions"
            );
        }
    }
}
