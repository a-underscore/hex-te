# hex-te

A GPU-rendered terminal emulator written in Rust with `winit` and `wgpu`, which
also hosts the engine core being ported from the Vulkano project in `../hex` (see
[Engine core](#engine-core-ported-from-hex)).

```sh
cargo run   # the terminal
cargo test  # terminal, engine and shader tests
```

## Current behavior

- Spawns a shell on a PTY — `$SHELL`, or the program named in the config — and
  renders what it prints: this is a shell-connected terminal emulator, not just a
  text widget.
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
- Holds its own resources — the GPU, the font, the grid and the shell — as
  components of one entity in an entity-component world, and handles events in a
  system that borrows them from the world.

Not implemented yet: SGR colours and attributes, wide (CJK) double-width cells,
scrollback, the alternate screen, mouse reporting, and text selection.

## Configuration

On first run the app writes a `config.py` into the config directory —
`$XDG_CONFIG_HOME/hex-te/config.py`, or `~/.config/hex-te/config.py` when
`XDG_CONFIG_HOME` is unset — and reads it back with an embedded Python
interpreter (`pyo3`). Nothing in it is required: a name that is missing or of the
wrong type keeps the default the file documents, and a file that cannot be
evaluated is reported while the defaults are used, so a broken config cannot stop
the terminal starting.

| Setting | Meaning |
| --- | --- |
| `font_size` | Size glyphs are rasterized at, in logical pixels |
| `window_width`, `window_height` | Size of a new window, in logical pixels |
| `shell` | Program to run inside the pty; `None` means `$SHELL` |
| `background`, `cursor_color` | Colours, as `(r, g, b)` floats in `0.0..=1.0` |

Because the file is Python, a setting can be computed rather than written out:

```python
import math

font_size = math.floor(15.7)
```

The file is read once, when the app starts, so restart it to pick up an edit. A
window manager is free to override the requested window size.

## Structure

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Creates the event loop, its user-event channel, and the application |
| `src/app.rs` | The winit glue: turns events into `Control` values and runs them through the systems |
| `src/config.rs` | Writes and evaluates the Python `config.py` through `pyo3` |
| `src/pty.rs` | Allocates the PTY, spawns `$SHELL`, and pumps its output from a reader thread |
| `src/terminal.rs` | The VT parser and character grid, key encoding, and glyph rasterization |
| `src/font.rs` | Loads the system monospace face and answers rasterization requests |
| `src/gpu.rs` | Configures the surface and renders the texture and cursor |
| `src/world/` | The ported engine: the entity/component store and the system manager |
| `src/components/` | The ported 3D components: `Camera3`, `Trans3`, `Tag` |
| `src/control.rs` | The event value a system is handed each turn, plus its `exit` flag |
| `src/id.rs` | The `Id` entity handle |
| `src/shaders/screen.wgsl` | Draws the screen texture and cursor overlay |

Data flows one way around the loop: keystrokes are encoded and written to the PTY,
the shell echoes and prints, the reader thread collects the bytes and wakes the
event loop, and the VT parser turns them into grid cells that `rasterize` uploads
as a texture.

## Engine core (ported from `hex`)

This is a single crate: the engine being ported from the Vulkano project in
`../hex` lives in `src/world/` and `src/components/`, and the terminal's own
resources are components of that same world.

The app owns exactly one entity, to which `Gpu`, `Font`, `Terminal` and `Pty` are
attached. Its behaviour is a single `System` (`TerminalSystem` in `src/app.rs`)
handed `(Control, World)` for every event; it borrows the components it needs out
of the world instead of storing them. `World::spawn`, `attach`, `attach_value`,
`component` and friends exist so callers pass a world rather than an entity
manager plus every component.

| Module | Notes |
| --- | --- |
| `world::World` | Entity-component store plus the global ambient lighting values |
| `world::{EntityManager, ComponentManager}` | Type-erased component storage behind `Arc<RwLock<C>>` |
| `world::{System, SystemManager}` | `init`/`update` units of behaviour, run in registration order |
| `control::Control` | The winit event plus an `exit` flag; a system sets `exit` to stop the loop |
| `components` | The engine's own components, so far unused by the terminal: `Camera3`, `Trans3`, `Tag` |

Two things are deliberately different from `hex` because the GPU API is: the
ambient values are handed over as `world::AmbientUniform`, a padding-free
`bytemuck::Pod` struct ready for `Queue::write_buffer` (the Vulkano original wrote
a descriptor subbuffer), and camera projections get a depth-only clip-space
correction, since wgpu's Y axis — unlike Vulkan's — already points up.

`Light3`, `Model` and `hex`'s `renderables/` and `renderers/` are still Vulkano
code and follow once the 3D renderer is ported.

The port lives on the `dev` branch; `master` still tracks the terminal-only
history.

## Requirements

- Rust edition 2024 toolchain.
- A GPU driver supported by `wgpu`.
- An installed monospace font, discovered through the system font database.
- A Python 3 installation with a shared `libpython`: the config file *is* Python,
  and `pyo3` embeds an interpreter to evaluate it. `pyo3` locates the interpreter
  through `python3` on `PATH` (or `PYO3_PYTHON`).

`cargo test` covers the terminal and the engine, and validates the shader and the
VT grid without needing a GPU or a PTY. The config tests need a Python
interpreter, and the tests that rasterize glyphs need a system monospace font.
