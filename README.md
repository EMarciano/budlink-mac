# budlink-mac

macOS menu bar app and CLI to monitor battery levels and control noise cancellation on Google Pixel Buds.

A macOS port of [budlink](https://github.com/neptunonet/budlink), using Maximilian Luz's [`maestro`](https://github.com/qzed/pbpctrl) crate over macOS `IOBluetooth` RFCOMM instead of Linux BlueZ. Tested on Pixel Buds 2a; other Maestro-based Pixel Buds (such as Pixel Buds Pro / Pro 2) should also work.

## Build & Install

Requires Rust and `protoc` (for `maestro`'s protobuf codegen):

```sh
brew install protobuf
cargo build --release
```

Pair your Pixel Buds in **System Settings → Bluetooth** first, and grant Bluetooth access when macOS prompts for it.

### Run at login (menu bar app)

```sh
cargo run --release -- install-agent   # installs to ~/.local/bin/budlink-mac and starts via launchd
budlink-mac agent-status               # check LaunchAgent state
budlink-mac uninstall-agent            # stop and remove
```

`install-agent` sets up a per-user LaunchAgent (`~/Library/LaunchAgents/budlink-mac.plist`, logs at `~/Library/Logs/budlink-mac.log`) that starts at login, reconnects automatically when the buds come in range, syncs on-bud touch-and-hold changes, and disables modes rejected by your model.

## CLI Usage

```sh
budlink-mac menubar      # run the menu bar app in the foreground
budlink-mac status       # battery levels, placement, and current noise control mode
budlink-mac anc <mode>   # set noise control: off | active | aware | adaptive
budlink-mac list         # list paired Bluetooth devices
budlink-mac probe        # list SDP services on the buds
budlink-mac settings     # dump firmware versions and ANC settings
```

Set `RUST_LOG=budlink_mac=debug` for verbose logging.

## Device Notes

- **In-ear detection**: The buds reject noise-control changes (`FailedPrecondition`) unless worn.
- **Case battery**: Reported only while at least one bud is in the case (the case has no Bluetooth radio).
- **Single control connection**: Pixel Buds accept one Maestro connection at a time; disconnect any phone Pixel Buds app if opening the channel times out.
- **Adaptive mode on Pixel Buds 2a**: Firmware-gated (`CurrentAncrState = Adaptive` is rejected even though the gesture-loop setting accepts `adaptive`); the menu automatically greys out rejected modes.

## How It Works

- **Protocol**: Google's Maestro protocol runs over Bluetooth Classic RFCOMM (UUID `25e97ff7-24ce-4c4c-8951-f764a708f7b5`: HDLC + Pigweed RPC + protobuf), handled by [`maestro`](https://github.com/qzed/pbpctrl) alongside active A2DP/HFP audio.
- **Transport**: Because macOS `CoreBluetooth` is BLE-only, `budlink-mac` opens the RFCOMM channel via `IOBluetooth` (`objc2-io-bluetooth`) and bridges delegate callbacks into a `tokio` `AsyncRead`/`AsyncWrite` stream.
- **Threading**: `IOBluetooth` and AppKit run on the main thread's `NSRunLoop` (avoiding `bluetoothd` XPC initialization deadlocks), while `tokio` drives the RPC session on a worker thread.

## License

[LGPL-3.0-or-later](LICENSE). Uses [`maestro`](https://github.com/qzed/pbpctrl) by Maximilian Luz (MIT/Apache-2.0) and is inspired by [budlink](https://github.com/neptunonet/budlink).

*Not affiliated with or endorsed by Google.*
