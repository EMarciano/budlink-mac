# budlink-mac

Control Google Pixel Buds (tested: Pixel Buds 2a) from macOS — a port-in-progress
of [budlink](https://github.com/neptunonet/budlink) (Linux, GTK4) to macOS.

Like the original, it talks to the buds over Google's Maestro protocol using
[`libmaestro`](https://github.com/qzed/pbpctrl) by Maximilian Luz, but replaces
the BlueZ RFCOMM transport with macOS's IOBluetooth framework.

> Status: early development. Not affiliated with or endorsed by Google.

## Usage

```
cargo run -- list         # list paired Bluetooth devices
cargo run -- probe        # dump the buds' SDP services (finds the Maestro channel)
cargo run -- status       # battery levels, placement, and noise control mode
cargo run -- anc <mode>   # set noise control: off | active | aware | adaptive
```

Pair and connect the buds in macOS Bluetooth settings first. The buds reject
noise-control changes (`FailedPrecondition`) unless they detect that they are
in your ears, and they report an undefined noise mode (raw 0) while unworn.
Case battery is only reported while at least one bud sits in the case (the
case has no Bluetooth radio of its own). The Pixel Buds 2a have no Adaptive
mode — requesting it is rejected by the buds (`Unknown` status), same as on
the original Linux app.

### Is the missing Adaptive mode a hardware or software limitation?

Investigated on 2026-11-05. Verdict: **firmware feature-gating (software),
not a documented hardware limitation.** Device-side evidence (see
`budlink-mac settings` and the hidden `x-gesture-adaptive` experiment):

- The 2a run firmware `release_4.467` — the same version that *delivered*
  Adaptive Audio to the Pixel Buds Pro 2 as a post-launch software update,
  proving the feature is firmware/DSP, not launch silicon.
- The 2a's settings schema accepts Adaptive: writing a gesture loop of
  `[active, aware, adaptive]` succeeds and reads back — only activating the
  mode (`CurrentAncrState = Adaptive`) is rejected (`Unknown`).
- Google's own docs attribute Adaptive Audio processing to the Tensor A1
  chip, which both models share.

Caveat: the 2a does have reduced ANC hardware (2 mics/bud vs 3, no
voice/motion sensors, "Silent Seal 1.5" vs 2.0), so a hardware-*quality*
rationale is conceivable — but no Google doc, teardown, or reviewer claims
Adaptive depends on those parts, and Google has never stated a rationale.

Set `RUST_LOG=budlink_mac=debug` for verbose logs.

## How it works

- **Protocol**: Maestro runs over Bluetooth *Classic* RFCOMM (service
  `MAESTRO APP`, UUID `25e97ff7-24ce-4c4c-8951-f764a708f7b5`, RFCOMM channel 1
  on the Pixel Buds 2a), framed as HDLC + Pigweed RPC + protobuf. `libmaestro`
  implements all of it; this crate only supplies a byte stream.
- **Transport**: CoreBluetooth is BLE-only, so the RFCOMM channel is opened
  with IOBluetooth (`IOBluetoothDevice.openRFCOMMChannelAsync`) via the
  `objc2-io-bluetooth` bindings. Delegate callbacks
  (`rfcommChannelData/OpenComplete/Closed`) feed a `tokio`
  `AsyncRead + AsyncWrite` wrapper that libmaestro's codec consumes. Writes go
  through `writeSync` chunked to the channel MTU.
- **Threading**: all IOBluetooth calls happen on the **main thread**, which
  then pumps its `NSRunLoop` forever (delegate callbacks are delivered there).
  The tokio runtime runs on a worker thread. Two pitfalls this avoids:
  - IOBluetooth's XPC handshake with `bluetoothd` can deadlock if first used
    from a background thread while the main thread is blocked.
  - `-[NSRunLoop run]` on a bare thread returns immediately when no sources
    are attached yet, so the loop uses `runMode:beforeDate:` instead.

Notably, macOS happily opens the RFCOMM channel while the buds also carry
A2DP/HFP audio — no extra privileges needed beyond the usual Bluetooth
permission prompt. (USB-C on the buds is charge-only; there is no control
channel over USB.)

## Dependencies

Build: Rust (2021 edition) and `protoc` (`brew install protobuf`) for
libmaestro's protobuf codegen.

## Roadmap

- [x] Device discovery, SDP probe, RFCOMM open
- [x] Battery levels (buds + case), placement info
- [x] Noise control read/write (off / ANC / transparency / adaptive)
- [ ] Follow device-side changes (touch-and-hold) via settings subscription
- [ ] Menu bar app (battery icon + noise control), like budlink's tray
- [ ] Reconnect handling when the buds come and go

## License

GPL-3.0-or-later. libmaestro is MIT/Apache-2.0 by Maximilian Luz.
