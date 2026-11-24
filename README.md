# Tessera

A GPU-accelerated terminal workspace in Rust: tabs, native split panes, an
optional tmux (control mode) backend for persistent sessions, and inline
history suggestions.

Named after the small tiles a mosaic is built from.

## Status

Under active construction. This repository is being built as a complete,
inspectable project: every layer has tests, and the UI is driven by GPUI
(`gpui` 0.2.x from Zed).

| Area | State |
|---|---|
| VT emulation core (`crates/tessera-term`) | in progress |
| Sessions, tabs, panes (`crates/tessera-core`) | in progress |
| History suggestions | in progress |
| tmux control-mode backend | planned |
| GPUI front end (`crates/tessera-ui`) | in progress |
| Linux / macOS / Windows | target |
| Android | core only; GPUI has no Android backend |

## Build

```sh
cargo run -p tessera-ui --release
cargo test
```

Linux needs Vulkan (or a software ICD such as lavapipe) for GPUI's renderer.

## Layout

```
crates/tessera-term   VT parser + grid + scrollback (no I/O, no pixels)
crates/tessera-core   PTY sessions, tabs/panes, tmux backend, suggestions, config
crates/tessera-ui     GPUI application: tab bar, pane layout, terminal element
docs/architecture.md  design notes, decisions and platform matrix
scripts/              shell integration for bash/zsh
```

## Why another terminal?

The goal is not to reimplement every terminal feature; it is a well-factored
terminal *workspace*: sessions that survive, panes that can be driven by tmux,
and a keyboard-driven UI that feels like an editor rather than a terminal
window. See `docs/architecture.md` for the reasoning and the trade-offs.
