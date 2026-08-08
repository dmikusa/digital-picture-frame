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
use std::path::PathBuf;

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

fn line_height(font_size: f32) -> f32 {
    font_size * 1.4
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
        let glyph_y = baseline - metrics.bounds.ymin;
        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha > 0 {
                    let px = (cx + metrics.bounds.xmin + col as f32) as u32;
                    let py = (glyph_y + row as f32) as u32;
                    if px < img.width() && py < img.height() {
                        let existing = img.get_pixel(px, py);
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
                        img.put_pixel(px, py, Rgb([r, g, b]));
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
    let (width, height) = config.resolution();
    let font = load_font()?;
    let white = Rgb([255u8, 255, 255]);

    // QR code in upper 55%
    let qr_url = format!("http://photo-frame:{}@{admin_url}", password);
    let code = QrCode::new(&qr_url).map_err(|e| format!("QR encode error: {e}"))?;
    let module_count = code.width() as u32;
    let qr_area = height * 55 / 100;
    let qr_size = qr_area.min(width * 8 / 10).max(32);
    let module_px = (qr_size / module_count).max(1);

    let qr_x = (width.saturating_sub(module_count * module_px)) / 2;
    let qr_pixel_h = module_count * module_px;
    let qr_y = (qr_area.saturating_sub(qr_pixel_h)) / 2;

    let mut img: RgbImage = ImageBuffer::new(width, height);
    for pixel in img.pixels_mut() {
        *pixel = Rgb([0u8, 0, 0]);
    }

    // Draw QR
    for y in 0..module_count {
        for x in 0..module_count {
            if code[(x as usize, y as usize)] == qrcode::Color::Dark {
                let x0 = qr_x + x * module_px;
                let y0 = qr_y + y * module_px;
                for dy in 0..module_px {
                    for dx in 0..module_px {
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

    // Text section: split remaining vertical space between QR bottom and footer
    let font_size = (height as f32 * 0.045).max(14.0);
    let lh = line_height(font_size);
    let footer_font = (height as f32 * 0.03).max(12.0);

    let qr_bottom = qr_y + qr_pixel_h;
    let footer_baseline = height as f32 - 10.0;
    let available = footer_baseline - qr_bottom as f32;
    let lines = 3.0;
    let text_block = lines * lh;
    let text_top = qr_bottom as f32 + (available - text_block).max(0.0) / 2.0;
    let baseline = text_top + font_size;

    let url_line = format!("http://{admin_url}");
    draw_centered(&mut img, baseline, &url_line, white, font_size, &font);

    let pw_line = format!("Password: {password}");
    draw_centered(&mut img, baseline + lh, &pw_line, white, font_size, &font);

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
        baseline + lh * 2.0,
        &ip_line,
        white,
        font_size,
        &font,
    );

    // Footer
    let footer_baseline = height as f32 - 10.0;
    draw_centered(
        &mut img,
        footer_baseline,
        "Insert USB to configure (remove to cancel)",
        white,
        footer_font,
        &font,
    );

    let out_path = PathBuf::from("/tmp/photo-frame-qr.jpg");
    DynamicImage::ImageRgb8(img)
        .save_with_format(&out_path, image::ImageFormat::Jpeg)
        .map_err(|e| format!("Failed to save config screen: {e}"))?;

    Ok(out_path)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let config = Config {
            photos_dir: PathBuf::from("/tmp/photos"),
            socket_path: PathBuf::from("/tmp/sock"),
            native_resolution: "800x600".to_string(),
            aspect_ratio_mode: crate::config::AspectRatioMode::Fit,
            batch_delete_size: 20,
            log_max_size: 262144,
            log_max_files: 2,
            remote_sources: vec![],
        };
        let path =
            render_config_screen(&config, "photo-frame.local:8147", "abc12345", "192.168.1.5")
                .unwrap();
        assert!(path.exists());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    }
}
