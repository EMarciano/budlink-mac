mod bt;

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;

use maestro::protocol::codec::Codec;
use maestro::protocol::types::RuntimeInfo;
use maestro::protocol::utils;
use maestro::pwrpc::client::{Client, ClientHandle};
use maestro::service::MaestroService;

use bt::{RfcommStream, StreamEvent};

const USAGE: &str = "budlink-mac — control Google Pixel Buds from macOS

usage:
  budlink-mac list     list paired Bluetooth devices
  budlink-mac probe    show SDP services of the buds (find the Maestro channel)
  budlink-mac battery  read battery level of buds and case
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
        "battery" => battery_main(),
        _ => {
            eprint!("{USAGE}");
            ExitCode::from(if cmd == "help" { 0 } else { 2 })
        }
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

/// Runs the battery command with the tokio runtime on a worker thread while
/// the main thread performs all IOBluetooth work and pumps its runloop.
fn battery_main() -> ExitCode {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<StreamEvent>();

    std::thread::Builder::new()
        .name("tokio".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime");
            let code = rt.block_on(async move {
                match battery(rx).await {
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

async fn battery(rx: UnboundedReceiver<StreamEvent>) -> Result<()> {
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

    let info = get_battery(handle, channel).await?;
    client_task.abort();

    let info = info
        .battery_info
        .expect("did not receive battery status in runtime-info-changed event");

    println!("Battery status:");
    for (label, entry) in [
        ("case ", info.case),
        ("left ", info.left),
        ("right", info.right),
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

    Ok(())
}

async fn get_battery(handle: ClientHandle, channel: u32) -> Result<RuntimeInfo> {
    println!("Reading battery info...");

    let mut service = MaestroService::new(handle, channel);
    let mut call = service.subscribe_to_runtime_info()?;
    let rt_info = if let Some(msg) = call.stream().next().await {
        msg?
    } else {
        bail!("did not receive any runtime-info event");
    };
    call.cancel_and_wait().await?;
    Ok(rt_info)
}
