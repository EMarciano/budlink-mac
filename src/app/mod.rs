//! Menu bar application: AppKit status item + connection worker.

mod ui;
mod worker;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Runs the menu bar app: AppKit on the main thread, tokio worker for the
/// protocol. Never returns until the user quits.
pub fn run() -> ExitCode {
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();

    let state = Arc::new(Mutex::new(ui::UiState::default()));
    let open_slot = Arc::new(Mutex::new(None));
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<worker::UiCommand>();

    let controller = ui::UiController::new(mtm, cmd_tx, state.clone(), open_slot.clone());
    let handle = ui::UiHandle::new(&controller, state, open_slot);
    std::mem::forget(controller); // lives for the app's lifetime

    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            rt.block_on(worker::worker(cmd_rx, handle));
        })
        .expect("failed to spawn tokio thread");

    app.run();
    ExitCode::SUCCESS
}
