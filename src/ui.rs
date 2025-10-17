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
use glib::{ControlFlow, ExitCode};
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box, Orientation, Picture, Stack, StackTransitionType};
use log::{debug, error, info, warn};
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
    debug!("Building UI with crossfade animation");

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

    // Create a Stack widget for crossfade animation between images
    let stack = Stack::builder()
        .transition_type(StackTransitionType::Crossfade)
        .transition_duration(1000) // 1 second crossfade
        .hhomogeneous(true)
        .vhomogeneous(true)
        .build();

    // Create two Picture widgets for double buffering the animation
    let picture1 = create_picture_widget();
    let picture2 = create_picture_widget();

    // Add both pictures to the stack with names
    stack.add_named(&picture1, Some("picture1"));
    stack.add_named(&picture2, Some("picture2"));

    // Start with picture1 visible
    stack.set_visible_child_name("picture1");

    // Load the first image into picture1
    load_image_into_picture(&picture1, &photo_loader, &memory_monitor);

    // Add the stack to the box (it will expand to fill available space)
    vbox.append(&stack);

    // Set the box as the window's child
    window.set_child(Some(&vbox));

    // Show the window
    window.present();

    // Set up automatic photo progression with crossfade every 5 seconds
    let stack_clone = stack.clone();
    let picture1_clone = picture1.clone();
    let picture2_clone = picture2.clone();
    let photo_loader_clone = photo_loader.clone();
    let memory_monitor_clone = memory_monitor.clone();
    let current_picture = Rc::new(RefCell::new(1)); // Track which picture is currently visible

    glib::timeout_add_local(std::time::Duration::from_secs(5), move || {
        debug!("Timer triggered - loading next photo with crossfade");
        
        let current = *current_picture.borrow();
        let (next_picture, next_name) = if current == 1 {
            (&picture2_clone, "picture2")
        } else {
            (&picture1_clone, "picture1")
        };

        // Load the next image into the hidden picture
        load_image_into_picture(next_picture, &photo_loader_clone, &memory_monitor_clone);

        // Trigger crossfade to the newly loaded picture
        stack_clone.set_visible_child_name(next_name);

        // Toggle the current picture tracker
        *current_picture.borrow_mut() = if current == 1 { 2 } else { 1 };

        ControlFlow::Continue
    });

    info!("UI built with crossfade animation and slideshow timer started");
}

fn create_picture_widget() -> Picture {
    let picture = Picture::new();
    picture.set_can_shrink(true);
    
    // Set content fit to scale down while preserving aspect ratio (GTK 4.8+)
    // For now, we'll just use the deprecated keep_aspect_ratio for compatibility
    picture.set_keep_aspect_ratio(true);
    
    picture.set_alternative_text(Some("Digital Picture Frame Display"));
    picture
}

fn load_image_into_picture(
    picture: &Picture,
    photo_loader: &Rc<RefCell<FilePhotoLoader>>,
    memory_monitor: &Rc<RefCell<MemoryMonitor>>,
) {
    let mut photo_loader_ref = photo_loader.borrow_mut();
    match photo_loader_ref.load_next_photo() {
        Ok(photo_url) => {
            debug!("Loading image: {}", photo_url);
            if let Ok(file_path) = photo_url.to_file_path() {
                let file = File::for_path(&file_path);
                picture.set_file(Some(&file));

                // Check memory after loading image
                let stats = memory_monitor.borrow_mut().check_memory();
                info!(
                    "Image loaded: {} - Memory: {} (growth: +{})",
                    file_path.display(),
                    MemoryMonitor::format_memory_human(stats.current_memory_kb),
                    MemoryMonitor::format_memory_human(stats.memory_growth_kb)
                );
            } else {
                error!("Failed to convert URL to file path: {}", photo_url);
                picture.set_alternative_text(Some("Failed to load image"));
            }
        }
        Err(e) => {
            warn!("Failed to load next photo: {} - cycling back to start", e);
            picture.set_alternative_text(Some("End of slideshow - restarting"));
        }
    }
}
