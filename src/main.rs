//! AudioPush — stream this PC's system audio to another PC on the LAN.
//!
//! One binary, two modes. See README.md for usage.

// Release builds have no console window; debug builds keep it for logging.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod audio;
mod config;
mod engine;
mod net;
mod pairing;
mod priority;
mod ring;
mod settings;
mod state;
mod tray;

use eframe::egui;

fn main() -> eframe::Result<()> {
    // 1 ms timer resolution for the whole process: the network threads nap in
    // sub-millisecond slices, and the default ~15.6 ms tick would turn those
    // naps into audible stalls. Held until the process exits.
    let _timer = priority::TimerResolution::acquire();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 700.0])
            .with_min_inner_size([460.0, 560.0])
            .with_title("AudioPush"),
        ..Default::default()
    };

    eframe::run_native(
        "AudioPush",
        options,
        Box::new(|cc| Ok(Box::new(app::AudioPushApp::new(cc)))),
    )
}
