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

use gio::File;
use glib::ExitCode;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box, Orientation, Picture};
use log::{debug, error, info};
use std::cell::RefCell;
use std::rc::Rc;

use crate::memory::MemoryMonitor;
use crate::photos::{FilePhotoLoader, PhotoLoader};

#[derive(Debug)]
pub enum UiErrors {
    InitializationError,
    RuntimeError,
}

pub fn run(
    photo_loader: FilePhotoLoader,
    memory_monitor: Rc<RefCell<MemoryMonitor>>,
) -> Result<(), UiErrors> {
    info!("Initializing GTK4 application");

    let app = Application::builder()
        .application_id("com.mikusa.picture-frame-ui")
        .build();

    let photo_loader = Rc::new(RefCell::new(photo_loader));
    let memory_monitor_clone = memory_monitor.clone();

    app.connect_activate(move |app| {
        build_ui(app, photo_loader.clone(), memory_monitor_clone.clone());
    });

    info!("Running GTK4 application");
    match app.run() {
        ExitCode::SUCCESS => {
            info!("GTK4 application exited successfully");
            Ok(())
        }
        _ => {
            error!("GTK4 application exited with error");
            Err(UiErrors::RuntimeError)
        }
    }
}

fn build_ui(
    app: &Application,
    photo_loader: Rc<RefCell<FilePhotoLoader>>,
    memory_monitor: Rc<RefCell<MemoryMonitor>>,
) {
    debug!("Building UI");

    // Create the main application window
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Digital Picture Frame")
        .default_width(800)
        .default_height(600)
        .build();

    // Create a vertical box to hold our UI elements
    let vbox = Box::builder()
        .orientation(Orientation::Vertical)
        .spacing(6)
        .build();
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // Create a Picture widget to display images
    let picture = Picture::new();
    picture.set_can_shrink(true);

    // Set content fit to scale down while preserving aspect ratio (GTK 4.8+)
    // For now, we'll just use the deprecated keep_aspect_ratio for compatibility
    picture.set_keep_aspect_ratio(true);

    picture.set_alternative_text(Some("Digital Picture Frame Display"));

    // Try to load the first available image
    let mut photo_loader_ref = photo_loader.borrow_mut();
    match photo_loader_ref.load_next_photo() {
        Ok(photo_url) => {
            debug!("Loading first image: {}", photo_url);
            if let Ok(file_path) = photo_url.to_file_path() {
                let file = File::for_path(&file_path);
                picture.set_file(Some(&file));

                // Check memory after loading first image
                let stats = memory_monitor.borrow_mut().check_memory();
                info!(
                    "Image loaded. Memory: {} (growth: +{})",
                    MemoryMonitor::format_memory_human(stats.current_memory_kb),
                    MemoryMonitor::format_memory_human(stats.memory_growth_kb)
                );
            } else {
                error!("Failed to convert URL to file path: {}", photo_url);
                picture.set_alternative_text(Some("Failed to load image"));
            }
        }
        Err(e) => {
            error!("Failed to load photo: {}", e);
            picture.set_alternative_text(Some("No images found"));
        }
    }

    // Add the picture to the box (it will expand to fill available space)
    vbox.append(&picture);

    // Set the box as the window's child
    window.set_child(Some(&vbox));

    // Show the window
    window.present();

    info!("UI built and presented successfully");
}
