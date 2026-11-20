# nagito

A low-latency PS5 player for Windows, written in Rust. It plays from a **capture card** or over **PS5 Remote Play** (via chiaki-ng), with sharper upscaling and optional 30 → 60 fps through your graphics driver. Works on AMD and NVIDIA.

## Build

Needs Rust (stable, `x86_64-pc-windows-gnu`) and MSYS2 with the UCRT64 packages from `.toolchain/setup.sh` (cmake, ninja, ffmpeg, SDL2, openssl, opus, json-c, miniupnpc, protobuf).

```
git submodule update --init --recursive
cargo run --release
```

`build.rs` builds `native/` (a small C library around chiaki-ng) with MSYS2 and copies the DLLs next to the exe. If MSYS2 is not at `.toolchain/msys64`, set `NAGITO_MSYS` to your `msys64` folder.

## Playing

**Capture card:** pick it under Source. The picture shows in the launcher right away, so you can check the signal and sharpening before you press Start.

**PS5 Remote Play:**
1. Turn on Remote Play on the PS5: Settings, System, Remote Play.
2. In nagito choose PlayStation 5. Your console shows up when it is on the same network.
3. Choose Sign in to PSN, sign in, then paste the address the browser ends on. That fills in your account ID.
4. Choose Link this console and type the code from the PS5's Link Device screen.
5. Connect a DualSense by USB or Bluetooth and press Start playing.

**Smooth 60** uses your driver's frame generation on nagito's window. Turn on Smooth Motion for `nagito.exe` in the NVIDIA app (RTX 40/50), or Fluid Motion Frames in AMD Software. It adds about one frame of delay. Responsive mode skips it for the lowest delay.

While playing: F1 shows stats, F11 switches fullscreen, Esc returns to the launcher.

## License

The Remote Play code is built from chiaki-ng, which is AGPL-3.0, so nagito is AGPL-3.0 too.
