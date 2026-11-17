use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass};
use objc2_foundation::NSObject;
use objc2_io_bluetooth::IOBluetoothDevice;
use tokio::sync::mpsc::UnboundedSender;

use super::worker::UiCommand;

struct Ivars {
    cmd_tx: UnboundedSender<UiCommand>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "BudlinkConnectObserver"]
    #[ivars = Ivars]
    struct ConnectObserver;

    unsafe impl NSObjectProtocol for ConnectObserver {}

    impl ConnectObserver {
        #[unsafe(method(bluetoothDeviceConnected:device:))]
        fn device_connected(&self, _notif: &AnyObject, device: &IOBluetoothDevice) {
            let name = unsafe { device.name() }.to_string();
            if name.contains("Pixel Buds") {
                tracing::info!(%name, "buds connected to the Mac");
                let _ = self.ivars().cmd_tx.send(UiCommand::Reconnect);
            }
        }
    }
);

pub fn register(cmd_tx: UnboundedSender<UiCommand>) {
    let this = ConnectObserver::alloc().set_ivars(Ivars { cmd_tx });
    let observer: Retained<ConnectObserver> = unsafe { msg_send![super(this), init] };
    let notif = unsafe {
        IOBluetoothDevice::registerForConnectNotifications_selector(
            Some(&*(Retained::as_ptr(&observer) as *const AnyObject)),
            Some(sel!(bluetoothDeviceConnected:device:)),
        )
    };
    if notif.is_none() {
        tracing::warn!("failed to register Bluetooth connect notifications");
    }
    std::mem::forget(observer);
    std::mem::forget(notif);
}
