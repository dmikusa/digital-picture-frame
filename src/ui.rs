use crate::photos::PhotoLoader;
use std::{
    result,
    time::{Duration, Instant, SystemTime},
};

use anyhow::Error;
use eframe::{Result, egui};
use egui::Image;
use egui_extras;
use log::{debug, error};

pub enum UiErrors {
    FailedToLoadPhoto(String, Error),
    UiError(Result),
}

pub fn run<T: PhotoLoader + 'static>(mut photo_loader: T) -> result::Result<(), UiErrors> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_active(true)
            .with_always_on_top()
            .with_fullscreen(true),
        ..Default::default()
    };

    // App State would go here
    let mut current_pic = match photo_loader.load_next_photo() {
        Ok(p) => p,
        Err(e) => {
            return Err(UiErrors::FailedToLoadPhoto(
                "Failed to load initial photo".to_string(),
                e,
            ));
        }
    };
    let mut next_pic: Option<url::Url> = None;
    let mut last_update = Instant::now();
    let mut transition_start: Option<Instant> = None;
    let photo_duration = Duration::from_secs(10);
    let fade_duration = Duration::from_secs(2);

    debug!(
        "Loaded initial photo: {} at {:?}",
        current_pic,
        SystemTime::now()
    );

    eframe::run_simple_native("Photo Frame UI", options, move |ctx, _frame| {
        egui_extras::install_image_loaders(ctx);

        // Check if it's time to start a transition
        if last_update.elapsed() > photo_duration && next_pic.is_none() {
            match photo_loader.load_next_photo() {
                Ok(p) => {
                    next_pic = Some(p.clone());
                    transition_start = Some(Instant::now());
                    debug!(
                        "Starting transition to new photo: {} at {:?}",
                        p,
                        SystemTime::now()
                    );
                }
                Err(e) => {
                    error!("Failed to load next photo: {}", e);
                }
            }
        }

        // Handle transition animation
        let (current_alpha, next_alpha) =
            if let (Some(next), Some(start)) = (&next_pic, transition_start) {
                let elapsed = start.elapsed();
                if elapsed >= fade_duration {
                    // Transition complete - swap photos
                    current_pic = next.clone();
                    next_pic = None;
                    transition_start = None;
                    last_update = Instant::now();
                    debug!("Transition complete, now showing: {}", current_pic);
                    (1.0, 0.0)
                } else {
                    // In transition - crossfade
                    let progress = elapsed.as_secs_f32() / fade_duration.as_secs_f32();
                    (1.0 - progress, progress) // Current fades out, next fades in
                }
            } else {
                (1.0, 0.0) // Normal display - current image fully visible
            };

        egui::CentralPanel::default().show(ctx, |ui| {
            // Use a custom painter for proper layering
            let rect = ui.available_rect_before_wrap();

            ui.centered_and_justified(|ui| {
                // Draw current image (fading out during transition)
                if current_alpha > 0.0 {
                    ui.add(
                        Image::new(current_pic.to_string())
                            .maintain_aspect_ratio(true)
                            .show_loading_spinner(false)
                            .tint(egui::Color32::from_white_alpha(
                                (current_alpha * 255.0) as u8,
                            )),
                    );
                }
            });

            // Draw next image on top if we're in transition (fading in)
            if let Some(next) = &next_pic
                && next_alpha > 0.0
            {
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.add(
                            Image::new(next.to_string())
                                .maintain_aspect_ratio(true)
                                .show_loading_spinner(false)
                                .tint(egui::Color32::from_white_alpha((next_alpha * 255.0) as u8)),
                        );
                    });
                });
            }
        });

        // Schedule repaints
        if next_pic.is_some() {
            ctx.request_repaint(); // Keep animating during transition
        } else {
            let time_until_next = photo_duration.saturating_sub(last_update.elapsed());
            ctx.request_repaint_after(time_until_next);
        }
    })
    .map_err(|e| UiErrors::UiError(Err(e)))
}
