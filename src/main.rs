#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use jl_explorer::app::{App, Startup};

/// Usage: jl-explorer [flash.bin] [--select <file name>]
fn main() -> eframe::Result {
    let mut startup = Startup::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--select" => startup.select = args.next(),
            _ => startup.open = Some(a.into()),
        }
    }

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([760.0, 480.0])
        .with_title("JL Explorer");
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native("JL Explorer", options, Box::new(|cc| Ok(Box::new(App::new(cc, startup)))))
}
