use std::ffi::c_void;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;

use anyhow::{anyhow, bail, Context as _, Result};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, Message};
use objc2_foundation::{NSObject, NSRunLoop};
use objc2_io_bluetooth::{
    BluetoothClassOfDevice, BluetoothRFCOMMChannelID, IOBluetoothDevice, IOBluetoothRFCOMMChannel,
    IOBluetoothRFCOMMChannelDelegate, IOBluetoothSDPDataElement, IOBluetoothSDPServiceRecord,
    IOBluetoothSDPUUID,
};

#[allow(non_camel_case_types)]
type IOReturn = std::ffi::c_int;

fn as_any_object<T: Message>(obj: &Retained<T>) -> &AnyObject {
    unsafe { &*(Retained::as_ptr(obj) as *const AnyObject) }
}

/// Class of Device values for the Pixel Buds (from pbpctrl).
const PIXEL_BUDS_CLASSES: [BluetoothClassOfDevice; 2] = [0x240404, 0x244404];

/// The RFCOMM channel is created on the main thread but only ever used for
/// `writeSync`/`closeChannel`, which IOBluetooth serializes internally — so it
/// is safe to send to (and call from) the worker thread. objc2 marks the class
/// `!Send`/`!Sync` by default; this wrapper asserts the narrower guarantee.
#[derive(Clone)]
pub struct Channel(Retained<IOBluetoothRFCOMMChannel>);

unsafe impl Send for Channel {}
unsafe impl Sync for Channel {}

impl std::ops::Deref for Channel {
    type Target = IOBluetoothRFCOMMChannel;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct OpenedChannel {
    pub channel: Channel,
    pub mtu: usize,
}

pub enum StreamEvent {
    /// Result of the openRFCOMMChannelAsync call itself.
    OpenResult(Result<OpenedChannel, IOReturn>),
    /// Async completion of the channel open.
    OpenComplete(IOReturn),
    Data(Vec<u8>),
    Closed,
    Error(String),
}

pub struct DelegateIvars {
    tx: UnboundedSender<StreamEvent>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "BudlinkRFCOMMDelegate"]
    #[ivars = DelegateIvars]
    struct BtDelegate;

    unsafe impl NSObjectProtocol for BtDelegate {}

    unsafe impl IOBluetoothRFCOMMChannelDelegate for BtDelegate {
        #[unsafe(method(rfcommChannelOpenComplete:status:))]
        unsafe fn open_complete(
            &self,
            _channel: Option<&IOBluetoothRFCOMMChannel>,
            status: IOReturn,
        ) {
            tracing::debug!(status, "rfcommChannelOpenComplete");
            let _ = self.ivars().tx.send(StreamEvent::OpenComplete(status));
        }

        #[unsafe(method(rfcommChannelData:data:length:))]
        unsafe fn data_received(
            &self,
            _channel: Option<&IOBluetoothRFCOMMChannel>,
            data_pointer: *mut c_void,
            data_length: usize,
        ) {
            let bytes =
                unsafe { std::slice::from_raw_parts(data_pointer as *const u8, data_length) };
            let _ = self.ivars().tx.send(StreamEvent::Data(bytes.to_vec()));
        }

        #[unsafe(method(rfcommChannelClosed:))]
        unsafe fn channel_closed(&self, _channel: Option<&IOBluetoothRFCOMMChannel>) {
            tracing::debug!("rfcommChannelClosed");
            let _ = self.ivars().tx.send(StreamEvent::Closed);
        }
    }
);

impl BtDelegate {
    fn new(tx: UnboundedSender<StreamEvent>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { tx });
        unsafe { msg_send![super(this), init] }
    }
}

fn maestro_sdp_uuid() -> Retained<IOBluetoothSDPUUID> {
    let bytes = maestro::UUID.as_bytes();
    unsafe { IOBluetoothSDPUUID::uuidWithBytes_length(bytes.as_ptr() as *const c_void, 16) }
        .expect("failed to create SDP UUID")
}

pub fn list_devices() -> Result<()> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let Some(devices) = IOBluetoothDevice::pairedDevices() else {
            println!("no paired devices");
            return Ok(());
        };
        for obj in devices.iter() {
            let Some(dev) = obj.downcast_ref::<IOBluetoothDevice>() else {
                continue;
            };
            let name = dev.name().to_string();
            let addr = dev
                .addressString()
                .map(|s| s.to_string())
                .unwrap_or_default();
            println!(
                "{addr}  connected={:5}  class=0x{:06x}  {name}",
                dev.isConnected(),
                dev.classOfDevice(),
            );
        }
        Ok(())
    })
}

unsafe fn find_buds() -> Result<Retained<IOBluetoothDevice>> {
    let Some(devices) = (unsafe { IOBluetoothDevice::pairedDevices() }) else {
        bail!("no paired devices");
    };
    // Name matches are authoritative; the class-of-device values are shared
    // with other earbuds (e.g. Nothing Ear), so they are only a fallback.
    let mut class_match: Option<Retained<IOBluetoothDevice>> = None;
    let mut buds: Option<Retained<IOBluetoothDevice>> = None;
    for obj in devices.iter() {
        let Some(dev) = obj.downcast_ref::<IOBluetoothDevice>() else {
            continue;
        };
        let name = unsafe { dev.name() }.to_string();
        let class = unsafe { dev.classOfDevice() };
        let by_name = name.contains("Pixel Buds");
        if !by_name && !PIXEL_BUDS_CLASSES.contains(&class) {
            continue;
        }
        tracing::debug!(%name, class = format_args!("0x{class:06x}"), by_name, "candidate device");
        if by_name {
            if unsafe { dev.isConnected() } {
                buds = Some(dev.retain());
                break;
            }
            if buds.is_none() {
                buds = Some(dev.retain());
            }
        } else if class_match.is_none() {
            class_match = Some(dev.retain());
        }
    }
    let dev = buds
        .or(class_match)
        .ok_or_else(|| anyhow!("no paired Pixel Buds found"))?;

    if !unsafe { dev.isConnected() } {
        tracing::info!("buds not currently connected; channel open will establish the link");
    }
    Ok(dev)
}

unsafe fn element_uuids(
    el: &IOBluetoothSDPDataElement,
    out: &mut Vec<Retained<IOBluetoothSDPUUID>>,
) {
    unsafe {
        if let Some(u) = el.getUUIDValue() {
            out.push(u);
        }
        if let Some(arr) = el.getArrayValue() {
            for obj in arr.iter() {
                if let Some(child) = obj.downcast_ref::<IOBluetoothSDPDataElement>() {
                    element_uuids(child, out);
                }
            }
        }
    }
}

unsafe fn record_uuids(rec: &IOBluetoothSDPServiceRecord) -> Vec<Retained<IOBluetoothSDPUUID>> {
    let mut out = Vec::new();
    unsafe {
        for attr in rec.sortedAttributes().iter() {
            let Some(attr) =
                attr.downcast_ref::<objc2_io_bluetooth::IOBluetoothSDPServiceAttribute>()
            else {
                continue;
            };
            if let Some(el) = attr.getDataElement() {
                element_uuids(&el, &mut out);
            }
        }
    }
    out
}

unsafe fn record_is_maestro(rec: &IOBluetoothSDPServiceRecord) -> bool {
    let target = maestro_sdp_uuid();
    unsafe { record_uuids(rec) }
        .iter()
        .any(|u| unsafe { u.isEqualToUUID(Some(&target)) })
}

unsafe fn find_maestro_channel(dev: &IOBluetoothDevice) -> Result<BluetoothRFCOMMChannelID> {
    let ret = unsafe { dev.performSDPQuery(None) };
    if ret != 0 {
        // Cached records are fine — the channel id rarely changes.
        tracing::debug!(ret, "SDP query failed, falling back to cached records");
    }

    let Some(services) = (unsafe { dev.services() }) else {
        bail!("device exposes no SDP services");
    };

    for obj in services.iter() {
        let Some(rec) = obj.downcast_ref::<IOBluetoothSDPServiceRecord>() else {
            continue;
        };
        let svc_name = unsafe { rec.getServiceName() }
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(unnamed)".into());
        tracing::debug!(service = %svc_name, "sdp service");

        if !unsafe { record_is_maestro(rec) } {
            continue;
        }

        let mut channel_id: BluetoothRFCOMMChannelID = 0;
        let ret = unsafe { rec.getRFCOMMChannelID(&mut channel_id) };
        if ret != 0 {
            bail!("Maestro service has no RFCOMM channel (IOReturn {ret})");
        }
        tracing::debug!(channel_id, "found Maestro RFCOMM channel");
        return Ok(channel_id);
    }

    bail!(
        "Maestro service (uuid {}) not found in SDP records",
        maestro::UUID
    )
}

pub fn probe() -> Result<()> {
    objc2::rc::autoreleasepool(|_| unsafe {
        let dev = find_buds()?;
        println!(
            "device: {} ({})",
            dev.name(),
            dev.addressString()
                .map(|s| s.to_string())
                .unwrap_or_default()
        );
        let ret = dev.performSDPQuery(None);
        if ret != 0 {
            bail!("SDP query failed (IOReturn {ret})");
        }
        let Some(services) = dev.services() else {
            bail!("no SDP services");
        };
        for obj in services.iter() {
            let Some(rec) = obj.downcast_ref::<IOBluetoothSDPServiceRecord>() else {
                continue;
            };
            let svc_name = rec
                .getServiceName()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(unnamed)".into());
            let is_maestro = record_is_maestro(rec);
            let mut ch: BluetoothRFCOMMChannelID = 0;
            let rfcomm = rec.getRFCOMMChannelID(&mut ch) == 0;
            println!(
                "  service: {svc_name:40} rfcomm={} maestro={is_maestro}",
                if rfcomm {
                    format!("ch{ch}")
                } else {
                    "-".into()
                }
            );
            for u in record_uuids(rec) {
                let desc: Retained<objc2_foundation::NSString> = msg_send![&*u, description];
                println!("      uuid: {desc}");
            }
        }
        Ok(())
    })
}

pub struct RfcommStream {
    rx: UnboundedReceiver<StreamEvent>,
    buf: BytesMut,
    channel: Channel,
    mtu: usize,
    eof: bool,
}

impl RfcommStream {
    pub fn new(rx: UnboundedReceiver<StreamEvent>, opened: OpenedChannel) -> Self {
        Self {
            rx,
            buf: BytesMut::new(),
            channel: opened.channel,
            mtu: opened.mtu,
            eof: false,
        }
    }
}

impl Drop for RfcommStream {
    fn drop(&mut self) {
        let ch = &*self.channel;
        unsafe {
            if ch.isOpen() {
                let _ = ch.setDelegate(None);
                let ret = ch.closeChannel();
                tracing::debug!(ret, "closed RFCOMM channel");
            }
        }
    }
}

impl AsyncRead for RfcommStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if !self.buf.is_empty() {
                let n = self.buf.len().min(buf.remaining());
                buf.put_slice(&self.buf.split_to(n));
                return Poll::Ready(Ok(()));
            }
            if self.eof {
                return Poll::Ready(Ok(()));
            }
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(StreamEvent::Data(bytes))) => {
                    self.buf.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(StreamEvent::Closed)) | Poll::Ready(None) => {
                    self.eof = true;
                }
                // Late open events; ignore once streaming.
                Poll::Ready(Some(_)) => continue,
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for RfcommStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.eof {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed")));
        }
        let n = buf.len().min(self.mtu).min(u16::MAX as usize);
        let ret = unsafe {
            self.channel
                .writeSync_length(buf.as_ptr() as *mut c_void, n as u16)
        };
        if ret != 0 {
            tracing::warn!(ret, "rfcomm writeSync failed");
            return Poll::Ready(Err(io::Error::other(format!(
                "rfcomm write failed (IOReturn {ret})"
            ))));
        }
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// Must be called on the main thread with a running runloop: IOBluetooth's
/// first use and its XPC handshake with bluetoothd are not safe when the main
/// thread is blocked elsewhere.
pub fn open_maestro_channel(tx: &UnboundedSender<StreamEvent>) {
    let res = autoreleasepool(|_| unsafe {
        (|| -> Result<()> {
            tracing::debug!("finding buds");
            let dev = find_buds()?;
            tracing::debug!("sdp query");
            let channel_id = find_maestro_channel(&dev)?;
            tracing::debug!(channel_id, "opening rfcomm channel");

            let delegate = BtDelegate::new(tx.clone());

            let mut channel: Option<Retained<IOBluetoothRFCOMMChannel>> = None;
            let ret = dev.openRFCOMMChannelAsync_withChannelID_delegate(
                Some(&mut channel),
                channel_id,
                Some(as_any_object(&delegate)),
            );
            tracing::debug!(ret, "openRFCOMMChannelAsync returned");

            let opened = match channel {
                Some(channel) if ret == 0 => {
                    let mtu = channel.getMTU() as usize;
                    Ok(OpenedChannel {
                        channel: Channel(channel),
                        mtu,
                    })
                }
                _ => Err(ret),
            };
            // The delegate must live as long as the runloop delivers callbacks.
            std::mem::forget(delegate);
            let failed = opened.is_err();
            let _ = tx.send(StreamEvent::OpenResult(opened));
            if failed {
                bail!("openRFCOMMChannelAsync failed (IOReturn {ret})");
            }
            Ok(())
        })()
    });

    if let Err(e) = res {
        let _ = tx.send(StreamEvent::Error(format!("{e:#}")));
    }
}

pub fn run_bt_main(tx: UnboundedSender<StreamEvent>) -> ! {
    open_maestro_channel(&tx);

    // `-[NSRunLoop run]` on a thread without sources yet returns immediately,
    // so pump the runloop manually.
    let run_loop = NSRunLoop::currentRunLoop();
    loop {
        let until = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(1.0);
        autoreleasepool(|_| {
            run_loop.runMode_beforeDate(unsafe { objc2_foundation::NSDefaultRunLoopMode }, &until);
        });
    }
}

pub async fn wait_for_open(mut rx: UnboundedReceiver<StreamEvent>) -> Result<RfcommStream> {
    let open = async {
        let opened = loop {
            match rx.recv().await {
                Some(StreamEvent::OpenResult(Ok(opened))) => break opened,
                Some(StreamEvent::OpenResult(Err(ret))) => {
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
        Ok(RfcommStream::new(rx, opened))
    };
    tokio::time::timeout(std::time::Duration::from_secs(30), open)
        .await
        .context(
            "timed out opening RFCOMM channel (the buds accept a single control connection: \
             close the Pixel Buds app on your phone, and check for a stale budlink-mac process)",
        )?
}
