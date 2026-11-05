//! Shared Maestro client setup for CLI commands.

use anyhow::Result;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use maestro::protocol::codec::Codec;
use maestro::protocol::utils;
use maestro::pwrpc::client::{Client, ClientHandle};
use maestro::pwrpc::Error as RpcError;

use crate::bt::{self, StreamEvent};

/// Opens the Maestro connection and starts the RPC client.
pub async fn setup_client(
    rx: UnboundedReceiver<StreamEvent>,
) -> Result<(ClientHandle, u32, JoinHandle<Result<(), RpcError>>)> {
    println!("Connecting to Maestro service...");
    let stream = bt::wait_for_open(rx).await?;
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
