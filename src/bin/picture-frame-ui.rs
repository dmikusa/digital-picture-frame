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

use log::{debug, error, info};
use picture_frame_ui::memory::MemoryMonitor;
use picture_frame_ui::photos::FilePhotoLoader;
use picture_frame_ui::ui;
use std::cell::RefCell;
use std::rc::Rc;

fn main() {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    // Initialize memory monitoring
    let memory_monitor = Rc::new(RefCell::new(MemoryMonitor::new()));
    let initial_stats = memory_monitor.borrow_mut().check_memory();
    info!(
        "Application started. Initial memory: {}",
        MemoryMonitor::format_memory_human(initial_stats.current_memory_kb)
    );

    debug!("Creating Photo Loader from test images directory");
    let photo_loader = FilePhotoLoader::new(String::from("test_images"));

    // Check memory after photo loader creation
    let after_loader_stats = memory_monitor.borrow_mut().check_memory();
    info!(
        "Photo loader created. Memory: {} (growth: +{})",
        MemoryMonitor::format_memory_human(after_loader_stats.current_memory_kb),
        MemoryMonitor::format_memory_human(after_loader_stats.memory_growth_kb)
    );

    debug!("Starting UI");
    match ui::run(photo_loader, memory_monitor.clone()) {
        Ok(_) => (),
        Err(e) => match e {
            ui::UiErrors::InitializationError => {
                error!("UI Initialization Error");
            }
            ui::UiErrors::RuntimeError => {
                error!("UI Runtime Error");
            }
        },
    }

    // Final memory check
    let final_stats = memory_monitor.borrow_mut().check_memory();
    info!(
        "Application finished. Final memory: {} (peak: {}, total growth: +{})",
        MemoryMonitor::format_memory_human(final_stats.current_memory_kb),
        MemoryMonitor::format_memory_human(final_stats.peak_memory_kb),
        MemoryMonitor::format_memory_human(final_stats.memory_growth_kb)
    );
}
