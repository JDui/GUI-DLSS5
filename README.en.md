# DLSS5 Neural Render v0.1.24

[中文](README.md) · [English](README.en.md) · [日本語](README.ja.md)

A desktop preview and export tool for DLSS Neural Rendering, built with Rust and Tauri v2.

## Preview

![DLSS5 Neural Render UI](Preview.jpg)

## Latest version

No compilation is required for normal use. Download the latest Windows x64 package from
[Releases](https://github.com/JDui/GUI-DLSS5/releases), extract it, and run `run.bat`.
The package includes the application, DLSS runtimes, FFmpeg, and FFprobe. NVIDIA GeForce
RTX 50, 40, and 30 series GPUs are supported.

## Build

```powershell
cargo build --release --manifest-path src-tauri\Cargo.toml
```

The executable is written to `src-tauri\target\release\dlss5-tauri.exe`. You can also run
`run.bat` from the project root; it builds the release executable when necessary and keeps
the correct working directory so the host DLLs and selected RTX runtime can be found.

## Built-in runtimes

- `nvngx_dlssnr.dll`: native RTX 50 runtime
- `nvngx_dlssnr_40.dll`: compatible RTX 40 runtime
- `nvngx_dlssnr_30.dll`: compatible RTX 30 runtime

The app detects a supported RTX 30 / 40 / 50 GPU at startup and selects the matching runtime.
If detection fails, RTX 50 is used by default and can be changed manually. Runtime changes
require an app restart because the NGX session is created per process.

## Languages

The interface initially follows the computer language when it is Chinese, English, or Japanese.
Other system languages fall back to Chinese. The pill-shaped language switcher at the top of
the window can change between 中文, English, and 日本語 at any time; the selected language is
remembered for the next launch.

## Interaction

- Import images or videos through the file picker, drag-and-drop, or the clipboard.
- PNG, JPG, GIF, MP4, AVI, MOV, MKV, and other common formats are supported. GIF input is
  baked to an MP4 cache in the `Temp` folder before frame preview and export.
- Video preview uses a persistent sequential decoder and a recent-frame cache. Frames can be
  skipped to keep playback aligned with the timeline, and FFmpeg / FFprobe subprocesses run
  silently in the background.
- Preview output is limited to 4520 pixels on its longer side. Image and video export use the
  selected output size, which defaults to the source size; X1 / X2 / X4 provide quick scaling.
- The right panel contains DLSS Parameters, Upscale, Post, and Encoder tabs.
- Multi-pass mode supports 1–5 DLSS passes. With actual upscaling, the order is DLSS pass 1 →
  RTX VSR once → the remaining DLSS passes. More passes can substantially increase noise;
  VSR can reduce it but cannot remove it completely.
- Upscaling uses RTX VSR and requires an RTX 20-series or newer GPU, a 550+ driver, and
  `nvngx_vsr.dll` plus `rtx_vsr_host.dll` in the program folder. When VSR is unavailable,
  upscaling is disabled and export cannot exceed the source resolution.
- The Encoder tab supports H.264 / H.265 through NVENC or CPU x264 / x265. Lower CQ / CRF
  values mean higher quality. H.265 / HEVC NVENC Lossless is available as a QP 0 lossless
  video option using a 4:4:4 RGB pixel format; if NVENC is unavailable, the export falls back
  to x265 lossless. Keep Audio preserves the source audio track by converting it to AAC, so
  the audio stream itself is not lossless.
- Quick Compare, Compare, and AB View are available. Hold the left mouse button in Quick
  Compare to temporarily show the original, drag the divider in Compare, and use the middle
  or right mouse button to pan. The wheel zooms around the pointer.

## Export

Choose a destination to export the current view, a DLSS image, or a DLSS video. The bundled
FFmpeg / FFprobe can process video and GIF files directly. When running from source, make sure
FFmpeg and FFprobe are available on `PATH`.

The app clears the previous `Temp` contents at startup and removes its temporary cache on a
normal shutdown.
