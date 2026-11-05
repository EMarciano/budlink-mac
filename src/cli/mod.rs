//! Command-line interface: argument parsing and the run harness.

mod client;
pub mod commands;

use std::future::Future;
use std::process::ExitCode;

use anyhow::Result;
use tokio::sync::mpsc::UnboundedReceiver;

use maestro::service::settings::AncState;

use crate::bt::{self, StreamEvent};

pub const USAGE: &str = "budlink-mac — control Google Pixel Buds from macOS

usage:
  budlink-mac list         list paired Bluetooth devices
  budlink-mac probe        show SDP services of the buds (find the Maestro channel)
  budlink-mac status       show battery levels and current noise control mode
  budlink-mac anc <mode>   set noise control: off | active | aware | adaptive
  budlink-mac settings     dump firmware info and ANC-related settings (diagnostic)
  budlink-mac menubar      run the menu bar app (battery icon + noise control)
";

/// Parses argv and runs the selected command, or `None` if the arguments
/// select the menu bar app instead.
pub fn run() -> Option<ExitCode> {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    let exit = match cmd.as_str() {
        "list" => report(bt::list_devices()),
        "probe" => report(bt::probe()),
        "status" => run_cmd(commands::status),
        "settings" => run_cmd(commands::settings_dump),
        "x-gesture-adaptive" => run_cmd(commands::gesture_adaptive_experiment),
        "menubar" => return None,
        "anc" => {
            let Some(mode) = std::env::args().nth(2) else {
                eprint!("{USAGE}");
                return Some(ExitCode::from(2));
            };
            let Some(state) = parse_anc_state(&mode) else {
                eprintln!("invalid mode '{mode}', expected off|active|aware|adaptive");
                return Some(ExitCode::from(2));
            };
            run_cmd(move |rx| commands::set_anc(rx, state))
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(if cmd == "help" { 0 } else { 2 })
        }
    };
    Some(exit)
}

fn parse_anc_state(mode: &str) -> Option<AncState> {
    match mode {
        "off" => Some(AncState::Off),
        "active" | "anc" => Some(AncState::Active),
        "aware" | "transparency" => Some(AncState::Aware),
        "adaptive" => Some(AncState::Adaptive),
        _ => None,
    }
}

fn report(res: Result<()>) -> ExitCode {
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs a command future on a worker-thread tokio runtime while the main
/// thread performs all IOBluetooth work and pumps its runloop.
fn run_cmd<F, Fut>(f: F) -> ExitCode
where
    F: FnOnce(UnboundedReceiver<StreamEvent>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            let code = rt.block_on(async move {
                match f(rx).await {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("Error: {e:#}");
                        1
                    }
                }
            });
            std::process::exit(code);
        })
        .expect("failed to spawn tokio thread");

    bt::run_bt_main(tx); // never returns
}
