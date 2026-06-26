use anyhow::Context;
use eframe::{NativeOptions, egui};
use endless_sketch::EndlessSketchApp;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let document_path = std::env::args_os().nth(1).map(PathBuf::from);
    let options = NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("EndlessSketch")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "EndlessSketch",
        options,
        Box::new(move |creation_context| {
            Ok(Box::new(
                EndlessSketchApp::new(creation_context, document_path.clone())
                    .context("failed to open the canvas")?,
            ))
        }),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))
}
