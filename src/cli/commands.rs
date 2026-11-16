use anyhow::{bail, Result};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;

use maestro::protocol::types::RuntimeInfo;
use maestro::service::settings::{AncState, SettingId, SettingValue};
use maestro::service::MaestroService;

use crate::bt::StreamEvent;

use super::client::setup_client;

pub async fn status(rx: UnboundedReceiver<StreamEvent>) -> Result<()> {
    let (handle, channel, client_task) = setup_client(rx).await?;
    let mut service = MaestroService::new(handle, channel);

    let info = read_battery(&mut service).await?;
    print_battery(&info);

    match read_anc_state(&mut service).await {
        Ok(state) => println!(
            "Noise control: {} (raw {})",
            state.as_str(),
            i32::from(state)
        ),
        Err(e) => println!("Noise control: unreadable ({e})"),
    }

    client_task.abort();
    Ok(())
}

pub async fn settings_dump(rx: UnboundedReceiver<StreamEvent>) -> Result<()> {
    let (handle, channel, client_task) = setup_client(rx).await?;
    let mut service = MaestroService::new(handle, channel);

    match service.get_software_info().await {
        Ok(sw) => {
            if let Some(fw) = sw.firmware {
                for (label, v) in [("case ", fw.case), ("left ", fw.left), ("right", fw.right)] {
                    match v {
                        Some(v) => println!("firmware {label}: {}", v.version_string),
                        None => println!("firmware {label}: unknown"),
                    }
                }
            }
        }
        Err(e) => println!("firmware info unreadable: {e}"),
    }

    println!("Settings:");
    for id in [
        SettingId::CurrentAncrState,
        SettingId::AncrStateOneBud,
        SettingId::AncrStateTwoBuds,
        SettingId::AncrGestureLoop,
        SettingId::AncAccessibilityMode,
        SettingId::MultipointEnable,
        SettingId::GestureEnable,
    ] {
        match service.read_setting_var(id).await {
            Ok(v) => println!("  {id:?}: {v:?}"),
            Err(e) => println!("  {id:?}: error ({e})"),
        }
    }

    client_task.abort();
    Ok(())
}

pub async fn set_anc(rx: UnboundedReceiver<StreamEvent>, state: AncState) -> Result<()> {
    let (handle, channel, client_task) = setup_client(rx).await?;
    let mut service = MaestroService::new(handle, channel);

    println!("Setting noise control to '{}'...", state.as_str());
    let res = service
        .write_setting(SettingValue::CurrentAncrState(state))
        .await;
    if let Err(e) = res {
        match e.code() {
            maestro::pwrpc::Status::Unknown => bail!(
                "the buds rejected '{}' — this mode is not supported on this model \
                 (Pixel Buds 2a have no Adaptive mode; that is Pro-only)",
                state.as_str()
            ),
            maestro::pwrpc::Status::FailedPrecondition => bail!(
                "{e} (hint: the buds usually reject noise-control changes \
                 unless they are in your ears)"
            ),
            _ => bail!(e),
        }
    }

    match read_anc_state(&mut service).await {
        Ok(current) => println!("Noise control is now: {}", current.as_str()),
        Err(e) => println!("Set, but reading back the state failed: {e}"),
    }

    client_task.abort();
    Ok(())
}

/// Checks whether 2a firmware gates Adaptive mode at the gesture-loop setting
/// level or only on state changes.
pub async fn gesture_adaptive_experiment(rx: UnboundedReceiver<StreamEvent>) -> Result<()> {
    let (handle, channel, client_task) = setup_client(rx).await?;
    let mut service = MaestroService::new(handle, channel);

    let original = match service.read_setting_var(SettingId::AncrGestureLoop).await? {
        SettingValue::AncrGestureLoop(l) => l,
        other => bail!("unexpected value: {other:?}"),
    };
    println!("original gesture loop: {original}");

    let mut modified = original;
    modified.adaptive = true;
    println!("writing gesture loop with adaptive enabled: {modified}");
    let res = service
        .write_setting(SettingValue::AncrGestureLoop(modified))
        .await;
    match &res {
        Ok(()) => println!("WRITE ACCEPTED"),
        Err(e) => println!("WRITE REJECTED: {e}"),
    }

    if res.is_ok() {
        match service.read_setting_var(SettingId::AncrGestureLoop).await? {
            SettingValue::AncrGestureLoop(l) => println!("read back: {l}"),
            other => println!("read back unexpected: {other:?}"),
        }
        service
            .write_setting(SettingValue::AncrGestureLoop(original))
            .await?;
        println!("restored original gesture loop");
    }

    client_task.abort();
    Ok(())
}

async fn read_battery(service: &mut MaestroService) -> Result<RuntimeInfo> {
    let mut call = service.subscribe_to_runtime_info()?;
    let rt_info = if let Some(msg) = call.stream().next().await {
        msg?
    } else {
        bail!("did not receive any runtime-info event");
    };
    call.cancel_and_wait().await?;
    Ok(rt_info)
}

fn print_battery(info: &RuntimeInfo) {
    if let Some(p) = &info.placement {
        let mut whereabouts = Vec::new();
        if p.left_bud_in_case {
            whereabouts.push("left bud in case");
        }
        if p.right_bud_in_case {
            whereabouts.push("right bud in case");
        }
        if !whereabouts.is_empty() {
            println!("Placement: {}", whereabouts.join(", "));
        }
    }
    let Some(battery) = &info.battery_info else {
        println!("Battery status: unknown");
        return;
    };
    println!("Battery status:");
    for (label, entry) in [
        ("case ", &battery.case),
        ("left ", &battery.left),
        ("right", &battery.right),
    ] {
        match entry {
            Some(i) => {
                let state = match i.state {
                    1 => "not charging".to_string(),
                    2 => "charging".to_string(),
                    x => format!("state {x}"),
                };
                println!("  {label}: {}% ({state})", i.level);
            }
            None => println!("  {label}: unknown"),
        }
    }
}

async fn read_anc_state(service: &mut MaestroService) -> Result<AncState> {
    match service
        .read_setting_var(SettingId::CurrentAncrState)
        .await?
    {
        SettingValue::CurrentAncrState(state) => Ok(state),
        other => bail!("unexpected settings value for ANC state: {other:?}"),
    }
}
