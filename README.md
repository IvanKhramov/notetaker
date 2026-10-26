# notetaker

An open-source AI notebook for online meetings, inspired by Granola and written
in Rust. Windows comes first, with macOS and Linux capture support planned.

**Pre-alpha:** the workspace includes an empty desktop window and an isolated
local ASR worker. Recording and AI note enhancement are not implemented yet.
See [the project plan](docs/PLAN.md) for architecture and milestones.

## Build

Install [Rust via rustup](https://rustup.rs/). The toolchain file selects stable
Rust and installs rustfmt, Clippy, and the Windows MSVC target.
On Debian/Ubuntu, install the [eframe prerequisites](https://github.com/emilk/egui/blob/0.37.0/crates/eframe/README.md)
and Wayland build tools:

```sh
sudo apt-get install pkg-config libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libxkbcommon-dev libssl-dev libwayland-dev
```

```sh
cargo build --workspace -j 2
cargo run -p notetaker-app
cargo run -p notetaker-cli -- --help
cargo test --workspace -j 2
```

## Local transcription

Build both binaries, then download the model (Silero VAD is installed with it):

```sh
cargo build -p notetaker-asr-worker -p notetaker-cli -j 2
./target/debug/notetaker-asr download gigaam-v3
./target/debug/notetaker-asr download parakeet-v3
./target/debug/notetaker transcribe input.wav
./target/debug/notetaker transcribe input.wav --model parakeet-v3
```

Input must be mono WAV at 8–192 kHz (integer PCM or float); the worker resamples
it to 16 kHz using sherpa. Output is one VAD segment
per line: `[00:01.234 - 00:03.456] text`. Timestamps describe speech chunks,
not individual words. GigaAM v3 with punctuation is the default Russian model;
Parakeet TDT v3 int8 supports English and 24 other European languages.
No audio leaves the machine; only the explicit `download` command uses the network.

Models go to `ProjectDirs("", "", "notetaker").data_local_dir()/models`:
`~/.local/share/notetaker/models` on Linux and
`%LOCALAPPDATA%\notetaker\data\models` on Windows. Override with
`NOTETAKER_MODELS_DIR`. Downloads are verified against pinned SHA-256 and size
before extraction, then installed atomically. Models and test WAVs are not in Git.
The CLI finds `notetaker-asr` alongside itself or on PATH; `NOTETAKER_ASR_BIN`
can select another executable.

See [worker documentation](crates/asr-worker/README.md) for the protocol, CUDA setup
and opt-in model integration tests.

## Third-party models

Weights have separate licenses from this application. Downloads are ONNX exports
from the [sherpa-onnx model releases](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models).

- `gigaam-v3`: Sber / Salute Developers, GigaAM v3 RNNT with punctuation,
  [MIT](https://github.com/salute-developers/GigaAM/blob/main/LICENSE),
  [upstream model](https://github.com/salute-developers/GigaAM); archive 170.2 MB.
- `parakeet-v3`: NVIDIA, Parakeet TDT 0.6B v3, quantized ONNX export,
  [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/),
  [original model and attribution](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3);
  archive 487.2 MB. Preserve NVIDIA attribution and indicate the ONNX/int8 conversion
  when redistributing these weights.
- `silero-vad`: Silero Team, [MIT](https://github.com/snakers4/silero-vad/blob/master/LICENSE),
  [upstream](https://github.com/snakers4/silero-vad); ONNX file 643.9 KB.

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
