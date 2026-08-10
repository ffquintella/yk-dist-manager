//! Binary entry point: initialise logging, open the data store, run the GUI.

use yk_dist_manager::{YkDistApp, logging, store::Store};

fn main() -> eframe::Result {
    logging::init();

    let data_path = Store::default_path();
    tracing::info!(event = "app.start", path = %data_path.display(), version = yk_dist_manager::VERSION);

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("YubiKey Distribution Manager"),
        ..Default::default()
    };

    eframe::run_native(
        "yk-dist-manager",
        options,
        Box::new(move |_cc| Ok(Box::new(YkDistApp::new(data_path)))),
    )
}
