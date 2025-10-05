/*
 * Digital Picture Frame - A fullscreen photo slideshow application
 * Copyright (C) 2025 Daniel Mikusa
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as published
 * by the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use log::{debug, error};
use picture_frame_ui::photos::FilePhotoLoader;
use picture_frame_ui::ui;

fn main() {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    debug!("Creating Photo Loader from base directory");
    let photo_loader = FilePhotoLoader::new(String::from("/Users/dmikusa/Pictures/BackgroundPics"));

    debug!("Starting UI");
    match ui::run(photo_loader) {
        Ok(_) => (),
        Err(e) => match e {
            ui::UiErrors::FailedToLoadPhoto(msg, err) => {
                error!("Photo Loading Error: {}: {}", msg, err);
            }
            ui::UiErrors::UiError(res) => {
                if let Err(err) = res {
                    error!("UI Error: {}", err);
                }
            }
        },
    }
}
