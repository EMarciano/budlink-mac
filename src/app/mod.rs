mod connect_notify;
mod single_instance;
mod ui;
mod worker;

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

pub fn run() -> ExitCode {
    let _guard = match single_instance::SingleInstance::acquire() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Error: {e:#}");
            return ExitCode::FAILURE;
        }
    };

    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();

    let state = Arc::new(Mutex::new(ui::UiState::default()));
    let open_slot = Arc::new(Mutex::new(None));
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<worker::UiCommand>();

    let controller = ui::UiController::new(mtm, cmd_tx.clone(), state.clone(), open_slot.clone());
    let handle = ui::UiHandle::new(controller, state, open_slot);

    connect_notify::register(cmd_tx);

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
