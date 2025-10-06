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

use std::{cell::RefCell, rc::Rc, result};

use iced::{
    Element, Length, Size, Task, Theme,
    widget::{column, container, image, text},
};
use log::{error, info};
use url::Url;

use crate::{memory::MemoryMonitor, photos::PhotoLoader};

pub enum UiErrors {
    InitializationError,
    RuntimeError,
}

#[derive(Debug, Clone)]
pub enum Message {
    ImageLoaded(Result<iced::widget::image::Handle, String>),
}

pub struct PictureFrameApp {
    current_image: Option<iced::widget::image::Handle>,
    loading_error: Option<String>,
}

impl PictureFrameApp {
    pub fn new(
        flags: (impl PhotoLoader + 'static, Rc<RefCell<MemoryMonitor>>),
    ) -> (Self, Task<Message>) {
        let (mut photo_loader, memory_monitor) = flags;

        // Load the first image
        let task = match photo_loader.load_next_photo_with_monitoring(Some(&memory_monitor)) {
            Ok(url) => {
                info!("Loading first image: {}", url);
                Task::perform(load_image_async(url), Message::ImageLoaded)
            }
            Err(e) => {
                error!("Failed to load first image: {}", e);
                Task::none()
            }
        };

        (
            Self {
                current_image: None,
                loading_error: None,
            },
            task,
        )
    }

    pub fn title(&self) -> String {
        "Digital Picture Frame".to_string()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ImageLoaded(result) => match result {
                Ok(handle) => {
                    self.current_image = Some(handle);
                    self.loading_error = None;
                    info!("Image loaded successfully");
                }
                Err(error) => {
                    self.loading_error = Some(error);
                    error!("Failed to load image: {:?}", self.loading_error);
                }
            },
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let content: Element<Message> = if let Some(ref image_handle) = self.current_image {
            // Display the image, scaling it to fit the window
            image(image_handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        } else if let Some(ref error) = self.loading_error {
            // Display error message
            text(format!("Error loading image: {}", error))
                .size(24)
                .into()
        } else {
            // Display loading message
            text("Loading image...").size(24).into()
        };

        container(column![content])
            .width(Length::Fill)
            .height(Length::Fill)
            .center(Length::Fill)
            .into()
    }

    pub fn theme(&self) -> Theme {
        Theme::Dark
    }
}

async fn load_image_async(url: Url) -> Result<iced::widget::image::Handle, String> {
    let start_time = std::time::Instant::now();

    match url.to_file_path() {
        Ok(path) => {
            info!("Reading image file: {}", path.display());
            let read_start = std::time::Instant::now();

            match tokio::fs::read(&path).await {
                Ok(data) => {
                    let read_duration = read_start.elapsed();
                    info!(
                        "File read completed in {:?} (size: {} bytes)",
                        read_duration,
                        data.len()
                    );

                    let decode_start = std::time::Instant::now();
                    let handle = iced::widget::image::Handle::from_bytes(data);
                    let decode_duration = decode_start.elapsed();

                    let total_duration = start_time.elapsed();
                    info!(
                        "Image decode completed in {:?}, total loading time: {:?}",
                        decode_duration, total_duration
                    );

                    Ok(handle)
                }
                Err(e) => Err(format!(
                    "Failed to read image file {}: {}",
                    path.display(),
                    e
                )),
            }
        }
        Err(_) => Err(format!("Invalid file URL: {}", url)),
    }
}

pub fn run<T: PhotoLoader + 'static>(
    photo_loader: T,
    memory_monitor: Rc<RefCell<MemoryMonitor>>,
) -> result::Result<(), UiErrors> {
    info!("Starting Iced-based picture frame UI");

    let window_settings = iced::window::Settings {
        size: Size::new(800.0, 600.0),
        position: iced::window::Position::Centered,
        ..Default::default()
    };

    match iced::application(
        "Digital Picture Frame",
        PictureFrameApp::update,
        PictureFrameApp::view,
    )
    .window(window_settings)
    .theme(PictureFrameApp::theme)
    .run_with(move || PictureFrameApp::new((photo_loader, memory_monitor)))
    {
        Ok(()) => {
            info!("Picture frame UI closed successfully");
            Ok(())
        }
        Err(e) => {
            error!("Picture frame UI error: {}", e);
            Err(UiErrors::RuntimeError)
        }
    }
}
