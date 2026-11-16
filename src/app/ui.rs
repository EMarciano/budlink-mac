use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSMenu, NSMenuItem,
    NSStatusBar, NSStatusItem,
};
use objc2_foundation::{NSObject, NSObjectNSThreadPerformAdditions, NSString};
use tokio::sync::mpsc::UnboundedSender;

use maestro::service::settings::AncState;

use crate::bt::{self, StreamEvent};

use super::worker::UiCommand;

#[derive(Clone, Copy, Debug)]
pub struct Batt {
    pub level: u8,
    pub charging: bool,
}

impl std::fmt::Display for Batt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.charging {
            write!(f, "{}% ⚡︎", self.level)
        } else {
            write!(f, "{}%", self.level)
        }
    }
}

#[derive(Clone, Default)]
pub struct UiState {
    pub connected: bool,
    pub connecting: bool,
    pub error: Option<String>,
    pub left: Option<Batt>,
    pub right: Option<Batt>,
    pub case: Option<Batt>,
    pub anc: Option<AncState>,
    /// Modes the buds have rejected (e.g. Adaptive on the 2a); shown disabled.
    pub rejected_modes: Vec<AncState>,
}

pub struct Ivars {
    state: Arc<Mutex<UiState>>,
    open_slot: Arc<Mutex<Option<UnboundedSender<StreamEvent>>>>,
    cmd_tx: UnboundedSender<UiCommand>,
    status_item: Retained<NSStatusItem>,
    header_item: Retained<NSMenuItem>,
    left_item: Retained<NSMenuItem>,
    right_item: Retained<NSMenuItem>,
    case_item: Retained<NSMenuItem>,
    mode_items: Vec<Retained<NSMenuItem>>,
    reconnect_item: Retained<NSMenuItem>,
    quit_item: Retained<NSMenuItem>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "BudlinkUiController"]
    #[ivars = Ivars]
    pub struct UiController;

    unsafe impl NSObjectProtocol for UiController {}

    impl UiController {
        #[unsafe(method(applyPendingState))]
        fn apply_pending_state(&self) {
            let state = self.ivars().state.lock().unwrap().clone();
            self.apply_state(&state);
        }

        #[unsafe(method(openChannel))]
        fn open_channel(&self) {
            let tx = self.ivars().open_slot.lock().unwrap().take();
            if let Some(tx) = tx {
                bt::open_maestro_channel(&tx);
            }
        }

        #[unsafe(method(modeSelected:))]
        fn mode_selected(&self, sender: &AnyObject) {
            let Some(item) = sender.downcast_ref::<NSMenuItem>() else {
                return;
            };
            let state = match item.tag() {
                1 => AncState::Off,
                2 => AncState::Active,
                3 => AncState::Aware,
                4 => AncState::Adaptive,
                _ => return,
            };
            let _ = self.ivars().cmd_tx.send(UiCommand::SetAnc(state));
        }

        #[unsafe(method(reconnectSelected:))]
        fn reconnect_selected(&self, _sender: &AnyObject) {
            let _ = self.ivars().cmd_tx.send(UiCommand::Reconnect);
        }

        #[unsafe(method(quitSelected:))]
        fn quit_selected(&self, _sender: &AnyObject) {
            let Some(mtm) = MainThreadMarker::new() else { return };
            NSApplication::sharedApplication(mtm).terminate(None);
        }
    }
);

impl UiController {
    pub fn new(
        mtm: MainThreadMarker,
        cmd_tx: UnboundedSender<UiCommand>,
        state: Arc<Mutex<UiState>>,
        open_slot: Arc<Mutex<Option<UnboundedSender<StreamEvent>>>>,
    ) -> Retained<Self> {
        let status_item = NSStatusBar::systemStatusBar()
            .statusItemWithLength(-1.0 /* NSVariableStatusItemLength */);
        if let Some(button) = status_item.button(mtm) {
            let symbol = NSString::from_str("earbuds");
            if let Some(image) =
                NSImage::imageWithSystemSymbolName_accessibilityDescription(&symbol, None)
            {
                image.setTemplate(true);
                button.setImage(Some(&image));
            }
            button.setTitle(NSString::from_str("…").as_ref());
        }

        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let header_item = NSMenuItem::new(mtm);
        header_item.setTitle(NSString::from_str("Pixel Buds").as_ref());
        header_item.setEnabled(false);
        menu.addItem(&header_item);

        let mk_info = |menu: &NSMenu| {
            let item = NSMenuItem::new(mtm);
            item.setEnabled(false);
            menu.addItem(&item);
            item
        };
        let left_item = mk_info(&menu);
        let right_item = mk_info(&menu);
        let case_item = mk_info(&menu);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let mut mode_items = Vec::new();
        for (tag, title) in [
            (1, "Off"),
            (2, "Active Noise Cancellation"),
            (3, "Transparency"),
            (4, "Adaptive"),
        ] {
            let item = NSMenuItem::new(mtm);
            item.setTitle(NSString::from_str(title).as_ref());
            item.setTag(tag);
            unsafe { item.setAction(Some(sel!(modeSelected:))) };
            item.setEnabled(false);
            menu.addItem(&item);
            mode_items.push(item);
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let reconnect_item = NSMenuItem::new(mtm);
        reconnect_item.setTitle(NSString::from_str("Reconnect").as_ref());
        unsafe { reconnect_item.setAction(Some(sel!(reconnectSelected:))) };
        menu.addItem(&reconnect_item);

        let quit_item = NSMenuItem::new(mtm);
        quit_item.setTitle(NSString::from_str("Quit budlink-mac").as_ref());
        unsafe { quit_item.setAction(Some(sel!(quitSelected:))) };
        menu.addItem(&quit_item);

        status_item.setMenu(Some(&menu));

        let this = Self::alloc().set_ivars(Ivars {
            state,
            open_slot,
            cmd_tx,
            status_item,
            header_item,
            left_item,
            right_item,
            case_item,
            mode_items,
            reconnect_item,
            quit_item,
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        unsafe {
            let target: &AnyObject = &*(Retained::as_ptr(&this) as *const AnyObject);
            for item in &this.ivars().mode_items {
                item.setTarget(Some(target));
            }
            this.ivars().reconnect_item.setTarget(Some(target));
            this.ivars().quit_item.setTarget(Some(target));
        }
        this
    }

    fn apply_state(&self, state: &UiState) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let ivars = self.ivars();

        let header = if state.connected {
            match &state.error {
                Some(e) => format!("Pixel Buds — {e}"),
                None => "Pixel Buds".to_string(),
            }
        } else if state.connecting {
            "Pixel Buds — connecting…".to_string()
        } else {
            match &state.error {
                Some(e) => format!("Pixel Buds — {e}"),
                None => "Pixel Buds — not connected".to_string(),
            }
        };
        ivars
            .header_item
            .setTitle(NSString::from_str(&header).as_ref());

        let fmt = |label: &str, b: Option<Batt>| match b {
            Some(b) => format!("{label}: {b}"),
            None => format!("{label}: —"),
        };
        ivars
            .left_item
            .setTitle(NSString::from_str(&fmt("Left", state.left)).as_ref());
        ivars
            .right_item
            .setTitle(NSString::from_str(&fmt("Right", state.right)).as_ref());
        ivars
            .case_item
            .setTitle(NSString::from_str(&fmt("Case", state.case)).as_ref());

        if let Some(button) = ivars.status_item.button(mtm) {
            let title = if state.connected {
                match (state.left, state.right) {
                    (Some(l), Some(r)) => format!("{}·{}", l.level, r.level),
                    (Some(l), None) => format!("{}", l.level),
                    (None, Some(r)) => format!("{}", r.level),
                    (None, None) => String::new(),
                }
            } else {
                String::new()
            };
            button.setTitle(NSString::from_str(&title).as_ref());
        }

        for item in &ivars.mode_items {
            let tag_state = match item.tag() {
                1 => Some(AncState::Off),
                2 => Some(AncState::Active),
                3 => Some(AncState::Aware),
                4 => Some(AncState::Adaptive),
                _ => None,
            };
            let on = state.connected && tag_state.is_some() && state.anc == tag_state;
            item.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            let rejected = tag_state.is_some_and(|s| state.rejected_modes.contains(&s));
            item.setEnabled(state.connected && !rejected);
        }

        ivars.reconnect_item.setEnabled(!state.connecting);
    }
}

#[derive(Clone)]
pub struct UiHandle {
    ctrl: MainThreadOnly<UiController>,
    state: Arc<Mutex<UiState>>,
    open_slot: Arc<Mutex<Option<UnboundedSender<StreamEvent>>>>,
}

/// Wraps a main-thread object so the worker can hold a `Retained` reference.
/// Sound to `Send` because the worker never touches the object directly and
/// only messages it via `performSelectorOnMainThread`, which AppKit serializes
/// onto the main thread.
struct MainThreadOnly<T: objc2::Message>(Retained<T>);
unsafe impl<T: objc2::Message> Send for MainThreadOnly<T> {}
impl<T: objc2::Message> Clone for MainThreadOnly<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl UiHandle {
    pub fn new(
        ctrl: Retained<UiController>,
        state: Arc<Mutex<UiState>>,
        open_slot: Arc<Mutex<Option<UnboundedSender<StreamEvent>>>>,
    ) -> Self {
        Self {
            ctrl: MainThreadOnly(ctrl),
            state,
            open_slot,
        }
    }

    fn call_main(&self, selector: objc2::runtime::Sel) {
        unsafe {
            self.ctrl
                .0
                .performSelectorOnMainThread_withObject_waitUntilDone(selector, None, false)
        };
    }

    pub fn update(&self, f: impl FnOnce(&mut UiState)) {
        f(&mut self.state.lock().unwrap());
        self.call_main(sel!(applyPendingState));
    }

    pub fn open_channel(&self, tx: UnboundedSender<StreamEvent>) {
        *self.open_slot.lock().unwrap() = Some(tx);
        self.call_main(sel!(openChannel));
    }
}
