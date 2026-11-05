//! budlink-mac — control Google Pixel Buds from macOS.
//!
//! Module layout:
//! - [`bt`]: macOS IOBluetooth ↔ tokio bridge (shared transport)
//! - [`cli`]: one-shot commands (list / probe / status / anc / settings)
//! - [`app`]: menu bar application (AppKit + connection worker)

mod app;
mod bt;
mod cli;

use std::process::ExitCode;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("budlink_mac=info")),
        )
        .init();

    cli::run().unwrap_or_else(app::run)
}
