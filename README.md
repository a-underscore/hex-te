# hex-te

A small GPU-rendered text-grid prototype written in Rust with `winit` and
`wgpu`.

```sh
cargo run
cargo test
```

## Current behavior

- Spawns `$SHELL` on a PTY and renders what it prints: this is a shell-connected
  terminal emulator, not just a text widget.
- Parses the shell's byte stream with a VT state machine (`vte`) into a character
  grid: `CR`, `LF`, `BS`, `TAB`, erasing, cursor movement, and scrolling all work.
- Forwards keystrokes to the shell: arrows, Home/End, Insert/Delete, PageUp/Down,
  `Ctrl`+letter as control codes, and `Alt`+key as an `ESC` prefix.
- The shell's own echo is the only thing drawn, so there is no double echo and
  the screen always agrees with the shell's idea of the current line.
- Reads from the PTY happen on a dedicated thread that wakes the event loop
  through a user event, so a quiet shell never blocks the UI.
- Resizes the PTY (`SIGWINCH`) to match the grid, and recomputes the grid from the
  window dimensions and font metrics on every redraw.
- Closes the window when the shell exits.

Not implemented yet: SGR colours and attributes, wide (CJK) double-width cells,
scrollback, the alternate screen, mouse reporting, and text selection.

## Structure

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Creates the event loop, its user-event channel, and the application |
| `src/app.rs` | Owns the window, PTY, and grid; dispatches input, output, and redraw events |
| `src/pty.rs` | Allocates the PTY, spawns `$SHELL`, and pumps its output from a reader thread |
| `src/terminal.rs` | The VT parser and character grid, key encoding, and glyph rasterization |
| `src/gpu.rs` | Configures the surface and renders the texture and cursor |
| `src/shaders/screen.wgsl` | Draws the screen texture and cursor overlay |

Data flows one way around the loop: keystrokes are encoded and written to the PTY,
the shell echoes and prints, the reader thread collects the bytes and wakes the
event loop, and the VT parser turns them into grid cells that `rasterize` uploads
as a texture.

## Library target (engine port)

Besides the terminal binary, the package builds a library (`src/lib.rs`) holding
the engine core being ported from the Vulkano project in `../hex`:

| Module | Notes |
| --- | --- |
| `world::World` | Entity-component store plus the global ambient lighting values |
| `components` | `Camera3`, `Trans3`, `Tag`; `Light3`/`Model` follow once the renderer does |
| `control::Control` | The winit event plus an `exit` flag, handed to each system |
| `world::System` | `init`/`update` units of per-frame behaviour, and their manager |

Anything a system needs is expected to live in the world, so a system is handed
the world (and the event) instead of a list of parameters — `World::spawn`,
`attach`, `component` and friends exist so callers hold a world rather than an
entity manager plus every component. The ambient values come out as
`world::AmbientUniform`, a padding-free `bytemuck::Pod` struct ready for
`Queue::write_buffer`; that layout is the one part that had to change from the
Vulkano original, which wrote a descriptor subbuffer.

The port lives on the `dev` branch; `master` still tracks the terminal-only
history, and the terminal binary does not use the library yet.

## Requirements

- Rust edition 2024 toolchain.
- A GPU driver supported by `wgpu`.
- An installed monospace font, discovered through the system font database.

`cargo test` covers the terminal and the engine, and validates the shader and the
VT grid without needing a GPU or a PTY. The tests that rasterize glyphs also need
a system monospace font.
