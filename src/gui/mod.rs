//! GUI-режим: окно egui + фоновый воркер пайплайна на tokio.

mod app;
pub mod messages;
mod worker;

use anyhow::{Context as _, Result};
use tokio::sync::mpsc;

/// Запускает окно приложения; блокирует до закрытия.
///
/// # Errors
/// Возвращает ошибку, если окно или tokio-рантайм не удалось создать.
pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([460.0, 640.0])
            .with_min_inner_size([360.0, 420.0])
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "Neptune",
        options,
        Box::new(|cc| {
            let (command_tx, command_rx) = mpsc::unbounded_channel();
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let events = worker::EventSender::new(event_tx, cc.egui_ctx.clone());

            let runtime =
                tokio::runtime::Runtime::new().context("не удалось создать tokio-рантайм")?;
            std::thread::Builder::new()
                .name("pipeline-worker".into())
                .spawn(move || runtime.block_on(worker::run(command_rx, events)))
                .context("не удалось запустить тред воркера")?;

            Ok(Box::new(app::NeptuneApp::new(command_tx, event_rx)))
        }),
    )
    .map_err(|error| anyhow::anyhow!("окно не запустилось: {error}"))
}
