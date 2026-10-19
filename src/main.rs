#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod app;
mod audio;
mod config;
mod dragout;
mod library;
mod ytdl;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SoundShelf")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([640.0, 320.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("SoundShelf", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
