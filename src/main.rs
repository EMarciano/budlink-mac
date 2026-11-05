mod bt;

use std::future::Future;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use maestro::protocol::codec::Codec;
use maestro::protocol::types::RuntimeInfo;
use maestro::protocol::utils;
use maestro::pwrpc::client::{Client, ClientHandle};
use maestro::pwrpc::Error as RpcError;
use maestro::service::settings::{AncState, SettingId, SettingValue};
use maestro::service::MaestroService;

use bt::{RfcommStream, StreamEvent};

const USAGE: &str = "budlink-mac — control Google Pixel Buds from macOS

usage:
  budlink-mac list         list paired Bluetooth devices
  budlink-mac probe        show SDP services of the buds (find the Maestro channel)
  budlink-mac status       show battery levels and current noise control mode
  budlink-mac anc <mode>   set noise control: off | active | aware | adaptive
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("budlink_mac=info".parse().unwrap()),
        )
        .init();

    let cmd = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match cmd.as_str() {
        "list" => report(bt::list_devices()),
        "probe" => report(bt::probe()),
        "status" => run_cmd(status),
        "anc" => {
            let Some(mode) = std::env::args().nth(2) else {
                eprint!("{USAGE}");
                return ExitCode::from(2);
            };
            let Some(state) = parse_anc_state(&mode) else {
                eprintln!("invalid mode '{mode}', expected off|active|aware|adaptive");
                return ExitCode::from(2);
            };
            run_cmd(move |rx| set_anc(rx, state))
        }
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(if cmd == "help" { 0 } else { 2 })
        }
    }
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

/// Waits for the channel open events and builds the stream.
async fn wait_for_open(mut rx: UnboundedReceiver<StreamEvent>) -> Result<RfcommStream> {
    let open = async {
        let (raw, mtu) = loop {
            match rx.recv().await {
                Some(StreamEvent::OpenResult(0, raw, mtu)) => break (raw, mtu),
                Some(StreamEvent::OpenResult(ret, _, _)) => {
                    bail!("openRFCOMMChannelAsync failed (IOReturn {ret})")
                }
                Some(StreamEvent::Error(e)) => bail!(e),
                Some(_) => continue,
                None => bail!("bluetooth main loop died before opening"),
            }
        };
        loop {
            match rx.recv().await {
                Some(StreamEvent::OpenComplete(0)) => break,
                Some(StreamEvent::OpenComplete(status)) => {
                    bail!("RFCOMM channel open failed (IOReturn {status})")
                }
                Some(StreamEvent::Error(e)) => bail!(e),
                Some(_) => continue,
                None => bail!("bluetooth main loop died before open completed"),
            }
        }
        Ok(RfcommStream::new(rx, raw, mtu))
    };
    tokio::time::timeout(Duration::from_secs(30), open)
        .await
        .context("timed out opening RFCOMM channel")?
}

/// Opens the Maestro connection and starts the RPC client.
async fn setup_client(
    rx: UnboundedReceiver<StreamEvent>,
) -> Result<(ClientHandle, u32, JoinHandle<Result<(), RpcError>>)> {
    println!("Connecting to Maestro service...");
    let stream = wait_for_open(rx).await?;
    println!("RFCOMM channel open");

    let stream = Codec::new().wrap(stream);
    let mut client = Client::new(stream);
    let handle = client.handle();

    let channel = utils::resolve_channel(&mut client).await?;

    let client_task = tokio::spawn(async move {
        let mut client = client;
        client.run().await
    });

    Ok((handle, channel, client_task))
}

async fn read_battery(
    service: &mut MaestroService,
) -> Result<RuntimeInfo> {
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
    match service.read_setting_var(SettingId::CurrentAncrState).await? {
        SettingValue::CurrentAncrState(state) => Ok(state),
        other => bail!("unexpected settings value for ANC state: {other:?}"),
    }
}

async fn status(rx: UnboundedReceiver<StreamEvent>) -> Result<()> {
    let (handle, channel, client_task) = setup_client(rx).await?;
    let mut service = MaestroService::new(handle, channel);

    let info = read_battery(&mut service).await?;
    print_battery(&info);

    match read_anc_state(&mut service).await {
        Ok(state) => println!("Noise control: {} (raw {})", state.as_str(), i32::from(state)),
        Err(e) => println!("Noise control: unreadable ({e})"),
    }

    client_task.abort();
    Ok(())
}

async fn set_anc(rx: UnboundedReceiver<StreamEvent>, state: AncState) -> Result<()> {
    let (handle, channel, client_task) = setup_client(rx).await?;
    let mut service = MaestroService::new(handle, channel);

    println!("Setting noise control to '{}'...", state.as_str());
    let res = service
        .write_setting(SettingValue::CurrentAncrState(state))
        .await;
    if let Err(e) = res {
        let hint = if format!("{e}").contains("FailedPrecondition") {
            " (hint: the buds usually reject noise-control changes unless they are in your ears)"
        } else {
            ""
        };
        bail!("{e}{hint}");
    }

    // Confirm what the buds report back.
    match read_anc_state(&mut service).await {
        Ok(current) => println!("Noise control is now: {}", current.as_str()),
        Err(e) => println!("Set, but reading back the state failed: {e}"),
    }

    client_task.abort();
    Ok(())
}
