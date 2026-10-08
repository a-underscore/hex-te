# hex-te

A GPU-rendered terminal emulator in Rust, built on `winit` and `wgpu`.

```sh
cargo run   # the terminal
cargo test  # unit and shader tests
```

## Current behavior

- Spawns a shell on a PTY — `$SHELL`, or the program named in the config — and
  renders what it prints: this is a shell-connected terminal emulator, not just a
  text widget.
- Parses the shell's byte stream with a VT state machine (`vte`) into a character
  grid: `CR`, `LF`, `BS`, `TAB`, erasing, cursor movement, and scrolling all work.
- Paints colour the way the shell asks for it: the 16 ANSI colours, the
  256-colour cube and its greys, and 24-bit truecolour, for foreground and
  background, with bold, dim, hidden, inverse, underline and strikethrough
  resolved as each cell is drawn.
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
  system that borrows them back out of that world.
- Lets the config file add systems of its own, so Python can shape the running
  app rather than only fill in settings.

Not implemented yet: italic, blink and a distinct bold face (those attributes
are parsed, but the loaded font has a single face), styled or coloured
underlines, wide (CJK) double-width cells, scrollback, the alternate screen,
mouse reporting, and text selection.

## Configuration

On first run the app writes a `config.py` into the config directory —
`$XDG_CONFIG_HOME/hex-te/config.py`, or `~/.config/hex-te/config.py` when
`XDG_CONFIG_HOME` is unset — and evaluates it with an embedded Python interpreter
(`pyo3`), so the file is Python and a value can be computed rather than written
out; a file that cannot be evaluated is reported instead of stopping the
terminal.

These are the names the file documents:

| Setting | Meaning |
| --- | --- |
| `font_size` | Size glyphs are rasterized at, in logical pixels |
| `window_width`, `window_height` | Size of a new window, in logical pixels |
| `shell` | Program to run inside the pty; `None` means `$SHELL` |
| `background`, `cursor_color` | Colours, as `(r, g, b)` floats in `0.0..=1.0` |

Reading those settings back is not wired up right now: the loader evaluates the
file for its effect on `world` (below), so the names above are documented but not
yet applied. The file is read once, when the app starts, so restart it to pick up
an edit; a window manager is free to override the requested window size.

### The world

The file is also handed the application's `world`, so a config can shape the
running app rather than only fill in settings:

| Call | Meaning |
| --- | --- |
| `world.spawn(active=True)` | Adds an entity, returning its id |
| `world.despawn(entity)` | Removes an entity and its components |
| `world.entities()`, `world.entity_count()` | The active entities, and how many there are |
| `world.add_system(fn, pipeline=0)` | Registers `fn(world)`, called once per event |
| `world.remove_system(pipeline=0)` | Drops the most recently added system |
| `world.system_count()` | How many systems are registered |
| `world.ambient_color`, `world.ambient_intensity` | The global lighting base values |

It is the same object the app runs on, not a copy, so anything it changes is
visible immediately. A system is an ordinary function taking the world, and it
may touch the world it is given:

```python
def dim(world):
    world.ambient_intensity = min(1.0, world.ambient_intensity + 0.01)

world.add_system(dim)
```

Systems run on the event loop's thread, once per event the app dispatches. A
system added while they run joins the next event, not the one in progress.

## Structure

| File | Responsibility |
| --- | --- |
| `src/main.rs` | Creates the event loop, its user-event channel, and the application |
| `src/app.rs` | The winit glue: turns events into `Control` values and runs them through the systems |
| `src/config.rs` | Writes and evaluates the Python `config.py` through `pyo3` |
| `src/pty.rs` | Allocates the PTY, spawns `$SHELL`, and pumps its output from a reader thread |
| `src/terminal.rs` | The VT parser and character grid, key encoding, and the rasterizer that paints cells and glyphs into the screen texture |
| `src/font.rs` | Loads the system monospace face and answers rasterization requests |
| `src/gpu.rs` | Configures the surface and renders the texture and cursor |
| `src/world/` | The entity-component world: the entity store, the system manager, and the ambient values |
| `src/control.rs` | The event value a system is handed each turn, plus its `exit` flag |
| `src/id.rs` | The `Id` entity handle |
| `src/shaders/screen.wgsl` | Draws the screen texture and cursor overlay |

Data flows one way around the loop: keystrokes are encoded and written to the PTY,
the shell echoes and prints, the reader thread collects the bytes and wakes the
event loop, and the VT parser turns them into grid cells that `rasterize` uploads
as a texture.

## The world

The app's resources and its behaviour share one entity-component world, in this
same crate: there is no library target in between.

The app owns exactly one entity, to which `Gpu`, `Font`, `Terminal` and `Pty` are
attached. Its behaviour is a single `System` (`TerminalSystem` in `src/app.rs`)
handed `(Control, World)` for every event; it borrows the components it needs out
of the world instead of storing them. `World::spawn`, `attach`, `attach_value`,
`component` and friends exist so callers pass a world rather than an entity
manager plus every component.

The world holds its systems as well as its entities — `World::add_system` and
`World::systems` — so `App` is only the winit glue that hands each event to them.
Each system sits behind its own handle and the manager is cloned out before the
systems run, which is what lets one borrow the world back as it runs; the Python
systems registered from the config file do exactly that.

| Module | Notes |
| --- | --- |
| `world::World` | Entity and system managers, plus the global ambient lighting values |
| `world::{EntityManager, ComponentManager}` | Type-erased component storage behind `Arc<RwLock<C>>` |
| `world::{System, SystemManager}` | `init`/`update` units of behaviour, added in order and run from a snapshot |
| `control::Control` | The winit event plus an `exit` flag; a system sets `exit` to stop the loop |

The ambient values reach a renderer as `world::AmbientUniform`: a padding-free
`bytemuck::Pod` struct laid out for a `vec3` plus an `f32`, ready for
`Queue::write_buffer`.

## Requirements

- Rust edition 2024 toolchain.
- A GPU driver supported by `wgpu`.
- An installed monospace font, discovered through the system font database.
- A Python 3 installation with a shared `libpython`: the config file *is* Python,
  and `pyo3` embeds an interpreter to evaluate it. `pyo3` locates the interpreter
  through `python3` on `PATH` (or `PYO3_PYTHON`).

`cargo test` covers the grid, the world and the config, and validates the shader
without needing a GPU or a PTY. The config tests need a Python interpreter, and
the tests that rasterize glyphs need a system monospace font.
