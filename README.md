# notetaker

An open-source AI notebook for online meetings, inspired by Granola and written
in Rust. Windows comes first, with macOS and Linux capture support planned.

**Pre-alpha:** M0 provides the workspace skeleton and an empty desktop window.
Recording, transcription, and AI features are not implemented yet.
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

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
