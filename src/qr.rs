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

/// Small bitmap font (5x8 pixels per character).
/// Each byte is a column, LSB at top.
const CHAR_W: u32 = 5;
const CHAR_H: u32 = 7;
const CHAR_SPACING: u32 = 1;

fn font_lookup(ch: char) -> Option<&'static [u8; 5]> {
    match ch {
        'A' => Some(&[0x7C, 0x12, 0x11, 0x12, 0x7C]),
        'B' => Some(&[0x7F, 0x49, 0x49, 0x49, 0x36]),
        'C' => Some(&[0x3E, 0x41, 0x41, 0x41, 0x22]),
        'D' => Some(&[0x7F, 0x41, 0x41, 0x22, 0x1C]),
        'E' => Some(&[0x7F, 0x49, 0x49, 0x49, 0x41]),
        'F' => Some(&[0x7F, 0x09, 0x09, 0x09, 0x01]),
        'G' => Some(&[0x3E, 0x41, 0x49, 0x49, 0x7A]),
        'H' => Some(&[0x7F, 0x08, 0x08, 0x08, 0x7F]),
        'I' => Some(&[0x41, 0x7F, 0x41, 0x00, 0x00]),
        'J' => Some(&[0x20, 0x40, 0x41, 0x3F, 0x01]),
        'K' => Some(&[0x7F, 0x08, 0x14, 0x22, 0x41]),
        'L' => Some(&[0x7F, 0x40, 0x40, 0x40, 0x40]),
        'M' => Some(&[0x7F, 0x02, 0x0C, 0x02, 0x7F]),
        'N' => Some(&[0x7F, 0x04, 0x08, 0x10, 0x7F]),
        'O' => Some(&[0x3E, 0x41, 0x41, 0x41, 0x3E]),
        'P' => Some(&[0x7F, 0x09, 0x09, 0x09, 0x06]),
        'Q' => Some(&[0x3E, 0x41, 0x51, 0x21, 0x5E]),
        'R' => Some(&[0x7F, 0x09, 0x19, 0x29, 0x46]),
        'S' => Some(&[0x46, 0x49, 0x49, 0x49, 0x31]),
        'T' => Some(&[0x01, 0x01, 0x7F, 0x01, 0x01]),
        'U' => Some(&[0x3F, 0x40, 0x40, 0x40, 0x3F]),
        'V' => Some(&[0x1F, 0x20, 0x40, 0x20, 0x1F]),
        'W' => Some(&[0x3F, 0x40, 0x38, 0x40, 0x3F]),
        'X' => Some(&[0x63, 0x14, 0x08, 0x14, 0x63]),
        'Y' => Some(&[0x07, 0x08, 0x70, 0x08, 0x07]),
        'Z' => Some(&[0x61, 0x51, 0x49, 0x45, 0x43]),
        'a' => Some(&[0x20, 0x54, 0x54, 0x54, 0x78]),
        'b' => Some(&[0x7F, 0x48, 0x44, 0x44, 0x38]),
        'c' => Some(&[0x38, 0x44, 0x44, 0x44, 0x20]),
        'd' => Some(&[0x38, 0x44, 0x44, 0x48, 0x7F]),
        'e' => Some(&[0x38, 0x54, 0x54, 0x54, 0x18]),
        'f' => Some(&[0x08, 0x7E, 0x09, 0x01, 0x02]),
        'g' => Some(&[0x0C, 0x52, 0x52, 0x52, 0x3E]),
        'h' => Some(&[0x7F, 0x08, 0x04, 0x04, 0x78]),
        'i' => Some(&[0x44, 0x7D, 0x40, 0x00, 0x00]),
        'j' => Some(&[0x20, 0x40, 0x44, 0x3D, 0x00]),
        'k' => Some(&[0x7F, 0x10, 0x28, 0x44, 0x00]),
        'l' => Some(&[0x41, 0x7F, 0x40, 0x00, 0x00]),
        'm' => Some(&[0x7C, 0x04, 0x18, 0x04, 0x78]),
        'n' => Some(&[0x7C, 0x08, 0x04, 0x04, 0x78]),
        'o' => Some(&[0x38, 0x44, 0x44, 0x44, 0x38]),
        'p' => Some(&[0x7C, 0x14, 0x14, 0x14, 0x08]),
        'q' => Some(&[0x08, 0x14, 0x14, 0x18, 0x7C]),
        'r' => Some(&[0x7C, 0x08, 0x04, 0x04, 0x08]),
        's' => Some(&[0x48, 0x54, 0x54, 0x54, 0x20]),
        't' => Some(&[0x04, 0x3F, 0x44, 0x40, 0x20]),
        'u' => Some(&[0x3C, 0x40, 0x40, 0x20, 0x7C]),
        'v' => Some(&[0x1C, 0x20, 0x40, 0x20, 0x1C]),
        'w' => Some(&[0x3C, 0x40, 0x30, 0x40, 0x3C]),
        'x' => Some(&[0x44, 0x28, 0x10, 0x28, 0x44]),
        'y' => Some(&[0x0C, 0x50, 0x50, 0x50, 0x3C]),
        'z' => Some(&[0x44, 0x64, 0x54, 0x4C, 0x44]),
        '0' => Some(&[0x3E, 0x51, 0x49, 0x45, 0x3E]),
        '1' => Some(&[0x42, 0x7F, 0x40, 0x00, 0x00]),
        '2' => Some(&[0x62, 0x51, 0x49, 0x49, 0x46]),
        '3' => Some(&[0x22, 0x41, 0x49, 0x49, 0x36]),
        '4' => Some(&[0x18, 0x14, 0x12, 0x7F, 0x10]),
        '5' => Some(&[0x27, 0x45, 0x45, 0x45, 0x39]),
        '6' => Some(&[0x3C, 0x4A, 0x49, 0x49, 0x30]),
        '7' => Some(&[0x01, 0x71, 0x09, 0x05, 0x03]),
        '8' => Some(&[0x36, 0x49, 0x49, 0x49, 0x36]),
        '9' => Some(&[0x06, 0x49, 0x49, 0x29, 0x1E]),
        ' ' => Some(&[0x00, 0x00, 0x00, 0x00, 0x00]),
        '.' => Some(&[0x40, 0x00, 0x00, 0x00, 0x00]),
        ':' => Some(&[0x14, 0x00, 0x00, 0x00, 0x00]),
        '/' => Some(&[0x20, 0x10, 0x08, 0x04, 0x02]),
        '-' => Some(&[0x08, 0x08, 0x08, 0x00, 0x00]),
        _ => None,
    }
}

fn draw_char_scaled(
    img: &mut RgbImage,
    x: u32,
    y: u32,
    ch: char,
    color: Rgb<u8>,
    scale: u32,
) -> Option<u32> {
    let glyph = font_lookup(ch)?;
    for (col, bits) in glyph.iter().enumerate() {
        for row in 0..7 {
            if (bits >> row) & 1 != 0 {
                let px = x + col as u32 * scale;
                let py = y + row as u32 * scale;
                for dy in 0..scale {
                    for dx in 0..scale {
                        let cx = px + dx;
                        let cy = py + dy;
                        if cx < img.width() && cy < img.height() {
                            img.put_pixel(cx, cy, color);
                        }
                    }
                }
            }
        }
    }
    Some(CHAR_W * scale)
}

fn text_width_scaled(text: &str, scale: u32) -> u32 {
    text.chars().filter_map(font_lookup).count() as u32 * (CHAR_W + CHAR_SPACING) * scale
}

fn draw_text_scaled(img: &mut RgbImage, x: u32, y: u32, text: &str, color: Rgb<u8>, scale: u32) {
    let mut cx = x;
    for ch in text.chars() {
        if let Some(w) = draw_char_scaled(img, cx, y, ch, color, scale) {
            cx += w + CHAR_SPACING * scale;
        }
    }
}

fn draw_centered_text_scaled(img: &mut RgbImage, y: u32, text: &str, color: Rgb<u8>, scale: u32) {
    let w = text_width_scaled(text, scale);
    let x = (img.width().saturating_sub(w)) / 2;
    draw_text_scaled(img, x, y, text, color, scale);
}

/// Generate a random 8-character alphanumeric password.
pub fn generate_password() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(char::from)
        .collect()
}

/// Generate a QR code JPEG at the given pixel dimensions.
pub fn generate_qr_jpeg(url: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let code = QrCode::new(url).map_err(|e| format!("QR encode error: {e}"))?;

    let module_count = code.width() as u32;

    // Scale QR to ~80% of the smaller screen dimension
    let qr_size = (width.min(height) * 8 / 10).max(32);
    let module_px = qr_size / module_count;

    let mut img: RgbImage = ImageBuffer::new(width, height);

    // Black background
    for pixel in img.pixels_mut() {
        *pixel = Rgb([0u8, 0, 0]);
    }

    let qr_x = (width.saturating_sub(module_count * module_px)) / 2;
    let qr_y = (height.saturating_sub(module_count * module_px)) / 2;

    // Draw QR modules as white blocks
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
    let dyn_img = DynamicImage::ImageRgb8(img);
    dyn_img
        .write_to(
            &mut std::io::Cursor::new(&mut jpeg_bytes),
            image::ImageFormat::Jpeg,
        )
        .map_err(|e| format!("JPEG encode error: {e}"))?;

    Ok(jpeg_bytes)
}

/// Render the full config-mode screen as a JPEG at native resolution.
/// Returns the path to the written JPEG file.
pub fn render_config_screen(
    config: &Config,
    admin_url: &str,
    password: &str,
    fallback_ip: &str,
) -> Result<PathBuf, String> {
    let (width, height) = config.resolution();

    // Build URL with basic auth for the QR code
    let qr_url = format!("http://photo-frame:{}@{admin_url}", password);

    let code = QrCode::new(&qr_url).map_err(|e| format!("QR encode error: {e}"))?;
    let module_count = code.width() as u32;

    // QR takes up the upper portion of the screen, centered vertically
    // within its allocated area.
    let qr_area_height = height * 55 / 100;
    let qr_size = qr_area_height.min(width * 8 / 10).max(32);
    let module_px = (qr_size / module_count).max(1);

    let qr_x = (width.saturating_sub(module_count * module_px)) / 2;
    let qr_pixel_h = module_count * module_px;
    let qr_y = (qr_area_height.saturating_sub(qr_pixel_h)) / 2;

    let mut img: RgbImage = ImageBuffer::new(width, height);

    // Black background
    for pixel in img.pixels_mut() {
        *pixel = Rgb([0u8, 0, 0]);
    }

    // Draw QR modules
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

    // Text below the QR code — scaled 3x for readability
    let white = Rgb([255u8, 255, 255]);
    let text_scale = 3u32;
    let text_line_h = (CHAR_H + 2) * text_scale;
    let text_y = qr_area_height + 20;

    let url_line = format!("http://{admin_url}");
    draw_centered_text_scaled(&mut img, text_y, &url_line, white, text_scale);

    let pw_line = format!("Password: {password}");
    draw_centered_text_scaled(&mut img, text_y + text_line_h, &pw_line, white, text_scale);

    let ip_line = format!(
        "Or: http://{fallback_ip}:{port}",
        port = {
            admin_url
                .split(':')
                .nth(1)
                .unwrap_or("8080")
                .split('/')
                .next()
                .unwrap_or("8080")
        }
    );
    draw_centered_text_scaled(
        &mut img,
        text_y + text_line_h * 2,
        &ip_line,
        white,
        text_scale,
    );

    // Footer
    let footer_y = height.saturating_sub(CHAR_H * 3 + 10);
    draw_centered_text_scaled(
        &mut img,
        footer_y,
        "Insert USB to configure (remove to cancel)",
        white,
        2,
    );

    let out_path = std::path::PathBuf::from("/tmp/photo-frame-qr.jpg");
    let dyn_img = DynamicImage::ImageRgb8(img);
    dyn_img
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
        // JPEG magic bytes
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn test_render_config_screen() {
        use std::path::PathBuf;
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
