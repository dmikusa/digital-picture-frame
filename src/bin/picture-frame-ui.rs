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
