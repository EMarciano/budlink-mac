mod bt;

use anyhow::{bail, Result};
use futures::StreamExt;

use maestro::protocol::codec::Codec;
use maestro::protocol::types::RuntimeInfo;
use maestro::protocol::utils;
use maestro::pwrpc::client::{Client, ClientHandle};
use maestro::service::MaestroService;

const USAGE: &str = "budlink-mac — control Google Pixel Buds from macOS

usage:
  budlink-mac list     list paired Bluetooth devices
  budlink-mac probe    show SDP services of the buds (find the Maestro channel)
  budlink-mac battery  read battery level of buds and case
";

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("budlink_mac=info".parse()?),
        )
        .init();

    let cmd = std::env::args().nth(1).unwrap_or_else(|| "help".into());
    match cmd.as_str() {
        "list" => bt::list_devices(),
        "probe" => bt::probe(),
        "battery" => battery().await,
        _ => {
            eprint!("{USAGE}");
            std::process::exit(if cmd == "help" { 0 } else { 2 });
        }
    }
}

async fn battery() -> Result<()> {
    println!("Connecting to Maestro service...");
    let stream = bt::connect_maestro().await?;
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
