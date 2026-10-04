# hex-te

A terminal emulator **template** built on [`wgpu`](https://wgpu.rs): windowing
with `winit`, PTY hosting with `portable-pty`, escape sequence parsing with
`vte`, and a single instanced `wgpu` pipeline that draws the entire screen.

It is meant to be read, run and then pulled apart: every module is small, has a
single job, and says so at the top.

```
cargo run          # opens a window and spawns $SHELL in it
cargo test         # 18 tests; none of them need a GPU
```

## What works today

* Real shell, real pty: `$SHELL` runs in a pty, window resizes send `SIGWINCH`.
* Text: 256 colour palette, 24-bit colour, bold / dim / italic / underline /
  strikethrough / reverse / hidden, bold-as-bright, double-width characters.
* Editing: wrapping, insert/delete char and line, erase char/line/display,
  scrolling regions (`DECSTBM`), tab stops, `DECCKM` cursor keys, saved cursor.
* Screens: the alternate screen (`DECSET 1049`) for `vim`, `less`, `htop`…
* History: 10 000 lines of scrollback, mouse wheel scrolling, cursor hidden
  while you scroll back.
* Input: the full `xterm` key table, `Ctrl+<char>`, `Alt` as an `ESC` prefix,
  `Shift+Tab`, function keys.
* Replies the shell can wait for: device attributes (`DA1`/`DA2`), device
  status (`DSR 5`/`6`), and `OSC 0`/`2` window titles.
* Fonts: discovered from the system font database, regular / bold / italic /
  bold-italic, lazily rasterized into one coverage atlas.

## Not implemented (good first patches)

Each of these has an obvious place to go:

| Feature | Where |
| --- | --- |
| Clipboard, paste, bracketed paste | `WindowEvent` handling in `app.rs` needs a clipboard crate; the terminal already tracks the mode (`NamedPrivateMode::BracketedPaste`) |
| Mouse reporting, selection, scrolling with a mouse | new module; `vte` already parses the modes, `terminal.rs` ignores them |
| Blinking cursor | `app.rs` needs a `ControlFlow::WaitUntil` timer + another `request_redraw` |
| Grapheme clusters, emoji, combining marks | `Terminal::put_char` currently drops zero-width characters |
| Reflow on resize | `Grid::resize` keeps the top-left corner instead of re-wrapping |
| Images (sixel / kitty), hyperlinks, `OSC 4`/`10`/`11` colour queries | `vte::ansi::Handler` has callbacks for all of them (`set_hyperlink`, `dynamic_color_sequence`, …) |

## Architecture

```
keyboard ──▶ input.rs ──▶ pty.rs ──▶ $SHELL
                                        │
                                        ▼
                        app.rs ──▶ terminal.rs   (grid, cursor, scrollback)
                                        │              ▲ vte::ansi::Handler
                                        ▼              │
                        paint.rs ──▶ cells become quads
                                        │
                                        ▼
                        renderer.rs ──▶ wgpu (one instanced draw call)
                                        ▲
                        glyphs.rs ──▶ fontdue rasterizer + coverage atlas
```

| File | Lines | Responsibility |
| --- | --- | --- |
| `src/main.rs` | 35 | Builds the event loop, hands over to `App` |
| `src/app.rs` | 386 | `ApplicationHandler`: window, GPU state, pty, resize, redraw |
| `src/terminal.rs` | 1044 | Cells, grid, scrollback, cursor, and the `vte` callbacks |
| `src/pty.rs` | 128 | Allocates the pty, spawns the shell, reader thread |
| `src/input.rs` | 260 | Key events → the bytes a terminal sends |
| `src/glyphs.rs` | 304 | Font loading, glyph rasterization, atlas packing |
| `src/paint.rs` | 194 | Grid → quads (colors, cursor, underline) |
| `src/renderer.rs` | 354 | The `wgpu` pipeline, atlas texture, instance buffer |
| `src/theme.rs` | 130 | The 256 entry palette and the default colors |
| `src/shaders/terminal.wgsl` | 75 | 6 vertices × 1 instance per quad |

### The three decisions worth knowing

1. **The parser lives outside the terminal.** `vte::ansi::Processor::advance`
   takes `&mut handler`, so the parser cannot be a field of the same struct it
   drives. `App` owns the parser, `Terminal` implements `Handler`, and the tests
   build their own parser. This is why `Terminal` has no `feed(&[u8])` method.

2. **One draw call for the whole screen.** Everything is a quad: background
   rectangles, glyphs, underlines, the cursor. The quad corners are generated
   from `vertex_index` in the shader, so per frame the CPU only uploads one
   `Instance` (64 bytes) per quad. Glyphs are sampled from a single-channel
   coverage atlas, which is re-uploaded only when a new glyph was rasterized.
   The painter emits backgrounds before text, because a glyph may overhang its
   cell and a neighbour's background would otherwise clip it.

3. **The renderer knows nothing about terminals.** `renderer.rs` draws
   `Instance`s; `paint.rs` decides what an `Instance` should be. Restyling the
   terminal, or reusing the renderer for something else, means touching one file.

## Requirements

* A Rust toolchain (edition 2024, so 1.85+) and a GPU driver that `wgpu` can
  talk to (Vulkan / Metal / DX12 / GL).
* A monospace font installed. `hex-te` asks the system font database for one; if
  your setup has none, point `HEX_TE_FONT` at a file:

  ```sh
  HEX_TE_FONT=/usr/share/fonts/MyMono-Regular.ttf cargo run
  ```

### If it exits with "no GPU adapter can present to this window"

`wgpu` is saying that no backend can drive a window surface. Its message lists
the backends it tried, which is usually enough to find the problem:

```
hex-te: no GPU adapter can present to this window. ... : No suitable graphics
adapter found; vulkan found no adapters, gl not compatible with provided surface
```

1. Find out what the system actually has: `nvidia-smi`, `glxinfo -B`,
   `vulkaninfo --summary` (on Gentoo those come from `x11-apps/mesa-progs` and
   `dev-util/vulkan-tools`). `RUST_LOG=wgpu=debug` logs why each adapter was
   rejected.
2. `vulkan found no adapters` means the loader has no ICD matching a *loaded*
   kernel driver. Compare `ls /usr/share/vulkan/icd.d/` with `lsmod`, and
   `cat /sys/class/drm/card0/device/uevent` to see which driver owns the GPU. A
   very common mix-up: only `nvidia_icd.json` is installed while the card is
   bound to `nouveau`, because the proprietary module was never loaded.
3. Mesa only builds the Vulkan drivers you ask for. On Gentoo the software
   rasterizer (lavapipe) and nouveau's Vulkan driver (nvk) are separate
   `VIDEO_CARDS` tokens:

   ```
   # /etc/portage/make.conf
   VIDEO_CARDS="nouveau lavapipe"    # or "nouveau nvk" for hardware acceleration
   ```
   ```
   sudo emerge -av media-libs/mesa   # then: WGPU_BACKEND=vulkan cargo run
   ```

4. `gl not compatible with provided surface` accompanied by `libEGL warning:
   egl: failed to create dri2 screen` means Mesa could not initialise its X11
   EGL/DRI path at all; `eglinfo -B` says the same thing with more detail.
   `LIBGL_ALWAYS_SOFTWARE=1` and
   `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json`
   pin it to Mesa's own driver instead of another vendor's EGL library.

Once one working driver exists, nothing else is needed: `hex-te` simply takes
whichever adapter `WGPU_BACKEND` selects.
