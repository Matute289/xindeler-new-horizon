---
name: xindeler-run
description: Use when launching the Veloren game client or server — knows the correct env vars, features, hot-reload behavior, and cargo aliases
---

# xindeler-run

## Pre-requisites

Always ensure the Rust toolchain is on PATH before running:
```bash
source "$HOME/.cargo/env"
```

`VELOREN_ASSETS` is resolved automatically when running from the project root (the build embeds the workspace path). Only set it manually when running a pre-built binary from a different directory or in CI:
```bash
export VELOREN_ASSETS="/path/to/xindeler/assets"
```

## Launching the Client (GUI)

**Standard dev build** (hot-reloading enabled for animations and UI):
```bash
cargo run --bin veloren-voxygen
```

**Minimal feature set** (omits discord, plugins, singleplayer — faster startup for net/logic testing):
```bash
cargo test-voxygen
```

**With Tracy profiler** (performance profiling):
```bash
cargo tracy-voxygen
```

**With debug symbols** (for lldb/gdb):
```bash
cargo dbg-voxygen
```

**Release build** (no hot-reloading, LTO, for shipping):
```bash
cargo build --release --no-default-features --features default-publish
./target/release/veloren-voxygen
```

## Launching the Server (Headless)

**Standard dev server** (with worldgen, hot-reload agent AI):
```bash
cargo server
# equivalent to: cargo run --bin veloren-server-cli
```

**Minimal server** (no hot-reloading, faster startup for testing net code):
```bash
cargo test-server
# equivalent to: cargo run --bin veloren-server-cli --no-default-features --features simd
```

**With Tracy profiler**:
```bash
cargo tracy-server
```

## Single-Player (Client embeds Server)

Single-player mode is built into `veloren-voxygen` via the `singleplayer` feature (on by default in dev). Just run the client — no separate server process needed:
```bash
cargo run --bin veloren-voxygen
# then select "Singleplayer" in the main menu
```

## Hot-Reloading Behavior

The `hot-reloading` feature is **on by default** in dev builds. It enables dynamic library reloading for:
- `server-agent` — NPC AI behavior (sub-feature: `hot-agent`, enabled by default in server)
- `voxygen-anim` — character animations (sub-feature: `hot-anim`, enabled by default in voxygen)

To pick up changes: save the file → the game reloads it automatically within a few seconds.

Hot-reloading is **disabled** in `default-publish` (release) builds.

## Configuration Files

- Server settings: `userdata/server/settings.ron` (created on first run, gitignored)
- Client settings: `userdata/client/settings.ron` (created on first run, gitignored)
- Cargo aliases: `.cargo/config.toml`

## Troubleshooting

- **"assets not found"**: Make sure `VELOREN_ASSETS="$(pwd)/assets"` is set and you're running from the project root.
- **Compilation errors on first run**: The project requires nightly Rust. Run `rustup show active-toolchain` — it should match the version in the `rust-toolchain` file at the project root.
- **Window doesn't appear**: Check macOS permissions (accessibility, network). Try running from terminal directly.
