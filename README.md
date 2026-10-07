# hex-te

A small GPU-rendered text-grid prototype written in Rust with `winit` and
`wgpu`.

```sh
cargo run
cargo test
```

## Current behavior

- Opens a window and rasterizes typed text with a system monospace font.
- Supports basic buffer editing with Backspace, Delete, arrows, Home, End,
  Enter, and Tab.
- Recalculates the number of character cells from the window dimensions and
  font metrics when it redraws.
- Draws the text texture and a grid-positioned cursor with a WGSL shader.

The shell and PTY helper exist, but PTY input/output is not yet connected to the
displayed text buffer. The current keyboard input edits an in-memory buffer;
this is not yet a shell-connected terminal emulator.

## Structure

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Creates the event loop and application |
| `src/app.rs` | Owns the window, handles input and redraw events |
| `src/terminal.rs` | Loads the font, edits the text buffer, computes the grid, and rasterizes glyphs |
| `src/gpu.rs` | Configures the surface and renders the texture and cursor |
| `src/pty.rs` | Creates a PTY and starts the configured shell; not yet wired to the display |
| `src/shaders/screen.wgsl` | Draws the screen texture and cursor overlay |

## Requirements

- Rust edition 2024 toolchain.
- A GPU driver supported by `wgpu`.
- An installed monospace font, discovered through the system font database.

`cargo test` validates the shader without requiring a GPU. The text-rasterization
tests also need a system monospace font.
