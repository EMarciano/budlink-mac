//! Connection worker: owns the Maestro session lifecycle on a tokio runtime,
//! mirroring budlink's device.rs session/serve structure.

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;

use maestro::protocol::codec::Codec;
use maestro::protocol::types::{settings_rsp, RuntimeInfo, SettingsRsp};
use maestro::protocol::utils;
use maestro::pwrpc::client::Client;
use maestro::service::settings::{id, AncState, SettingValue};
use maestro::service::MaestroService;

use crate::bt::{self, StreamEvent};

use super::ui::{Batt, UiHandle, UiState};

pub enum UiCommand {
    SetAnc(AncState),
    Reconnect,
}

/// Why a session ended.
enum SessionEnd {
    /// User asked to quit (command channel closed).
    Quit,
    /// User asked to reconnect now.
    Reconnect,
}

/// Connection supervisor: (re)connects, serves, and retries with backoff.
pub async fn worker(mut cmd_rx: UnboundedReceiver<UiCommand>, ui: UiHandle) {
    loop {
        ui.update(|s| {
            s.connected = false;
            s.connecting = true;
            s.error = None;
        });

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();
        ui.open_channel(tx);

        match session(rx, &mut cmd_rx, &ui).await {
            Ok(SessionEnd::Quit) => return,
            Ok(SessionEnd::Reconnect) => continue,
            Err(e) => {
                tracing::warn!(error = ?e, "session ended");
                // Both mean the buds are out of reach right now: a pending
                // channel open, or the Maestro service not yet in SDP records
                // (buds linked but services not resolved yet). Both are
                // transient — stay quiet and retry fast.
                let err_chain = format!("{e:#}");
                let waiting = err_chain.contains("timed out opening")
                    || err_chain.contains("Maestro service");
                ui.update(|s| {
                    s.connected = false;
                    // A pending open just means the buds are not linked to this
                    // Mac right now — keep the quiet "connecting" state.
                    s.connecting = waiting;
                    s.anc = None;
                    s.error = if waiting { None } else { Some(short_error(&e)) };
                });

                // Auto-retry, faster when we're just waiting for the buds to
                // link (pending open) vs. a real protocol error; or
                // immediately on Reconnect.
                let delay = if waiting {
                    Duration::from_secs(3)
                } else {
                    Duration::from_secs(10)
                };
                tokio::select! {
                    cmd = cmd_rx.recv() => match cmd {
                        Some(UiCommand::Reconnect) => continue,
                        Some(UiCommand::SetAnc(_)) => continue, // retry anyway
                        None => return,
                    },
                    _ = tokio::time::sleep(delay) => continue,
                }
            }
        }
    }
}

/// Menu-friendly one-liner for a session error; technical detail stays in
/// the logs (RUST_LOG).
fn short_error(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    if s.contains("no paired Pixel Buds")
        || s.contains("timed out opening")
        || s.contains("Maestro service")
    {
        return "buds not found".to_string();
    }
    if s.contains("single control connection") || s.contains("IO stream closed") {
        return "connection lost".to_string();
    }
    s.chars().take(40).collect()
}

async fn session(
    rx: UnboundedReceiver<StreamEvent>,
    cmd_rx: &mut UnboundedReceiver<UiCommand>,
    ui: &UiHandle,
) -> Result<SessionEnd> {
    let stream = bt::wait_for_open(rx).await?;

    let mut client = Client::new(Codec::new().wrap(stream));
    let channel = utils::resolve_channel(&mut client).await?;
    let handle = client.handle();

    tracing::info!("connected");
    ui.update(|s| {
        s.connected = true;
        s.connecting = false;
        s.error = None;
        s.rejected_modes.clear();
    });

    let service = MaestroService::new(handle, channel);

    tokio::select! {
        res = client.run() => {
            res?;
            bail!("client stopped unexpectedly");
        }
        res = serve(service, cmd_rx, ui) => res,
    }
}

/// Pushes device state to the UI and applies UI commands until something fails.
async fn serve(
    mut service: MaestroService,
    cmd_rx: &mut UnboundedReceiver<UiCommand>,
    ui: &UiHandle,
) -> Result<SessionEnd> {
    let anc = service
        .read_setting(id::CurrentAncrState)
        .await
        .context("initial ANC read")?;
    ui.update(|s| s.anc = Some(anc));

    let mut runtime_call = service.clone().subscribe_to_runtime_info()?;
    let mut settings_call = service.clone().subscribe_to_settings_changes()?;
    let mut runtime = runtime_call.stream();
    let mut changes = settings_call.stream();

    loop {
        tokio::select! {
            info = runtime.next() => {
                let info = info.context("runtime info stream ended")??;
                ui.update(|s| battery_from(&info, s));
            }
            change = changes.next() => {
                let change = change.context("settings stream ended")??;
                if let Some(SettingValue::CurrentAncrState(state)) = setting_from(change) {
                    ui.update(|s| s.anc = Some(state));
                }
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(UiCommand::SetAnc(state)) => {
                    let res = service
                        .write_setting(SettingValue::CurrentAncrState(state))
                        .await;
                    if let Err(err) = res {
                        tracing::warn!(error = ?err, ?state, "buds rejected ANC mode");
                        ui.update(|s| {
                            s.rejected_modes.push(state);
                            // Optimistic checkmark must be rolled back.
                        });
                        let current = service
                            .read_setting(id::CurrentAncrState)
                            .await
                            .context("ANC read-back after rejection")?;
                        ui.update(|s| s.anc = Some(current));
                    }
                }
                Some(UiCommand::Reconnect) => return Ok(SessionEnd::Reconnect),
                None => return Ok(SessionEnd::Quit),
            },
        }
    }
}

fn setting_from(rsp: SettingsRsp) -> Option<SettingValue> {
    let settings_rsp::ValueOneof::Value(value) = rsp.value_oneof?;
    Some(value.value_oneof?.into())
}

fn battery_from(info: &RuntimeInfo, state: &mut UiState) {
    let conv = |b: &maestro::protocol::types::DeviceBatteryInfo| Batt {
        level: b.level.clamp(0, 100) as u8,
        charging: b.state == 2,
    };

    let bat = info.battery_info.as_ref();
    state.left = bat.and_then(|b| b.left.as_ref()).map(conv);
    state.right = bat.and_then(|b| b.right.as_ref()).map(conv);
    state.case = bat.and_then(|b| b.case.as_ref()).map(conv);
}
