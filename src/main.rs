use app::Application;
use tracing::Level;

mod app;

#[tokio::main]
async fn main() -> Result<(), eframe::Error> {
    // let console_layer = console_subscriber::spawn();

    tracing_subscriber::fmt()
        .with_max_level(Level::DEBUG)
        .init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 300.0])
            .with_min_inner_size([300.0, 220.0]),

        ..Default::default()
    };

    eframe::run_native(
        "File sharing",
        native_options,
        Box::new(|cc| Ok(Box::new(Application::new(cc)))),
    )
}
