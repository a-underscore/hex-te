use crate::font::Font;
use crate::glyphs::{self, Rect};
use crate::pty::{INITIAL_COLS, INITIAL_ROWS};
use crate::{WINDOW_TITLE, gpu::Gpu};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use vte::{Params, Perform};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// How long each half of a blink lasts: the cursor and a cell that asked for
/// `SGR 5` are shown for one half-cycle and hidden for the next.
const BLINK_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) struct Terminal {
    pub cursor_position: (usize, usize),
    pub cursor_size: (f32, f32),
    /// Whether the cursor is painted this frame: the shell can hide it with
    /// `DECTCEM`, and a blinking cursor is hidden on alternate half-cycles.
    pub cursor_visible: bool,
    /// The cursor's shape, in the shader's `cursor_style` constants.
    pub cursor_style: u32,
    pub size: (usize, usize),
    pub last_texture_size: wgpu::Extent3d,
    pub texture: Option<wgpu::Texture>,
    /// A handle to the font component rather than a copy of it: the grid asks it
    /// for advances and glyph bitmaps, and nothing else owns those settings.
    font: Arc<RwLock<Font>>,
    /// The colours the grid is painted with.
    palette: Palette,
    screen: Screen,
    /// The clock the cursor and every `SGR 5` cell share.
    blink: Blink,
    parser: vte::Parser,
}

impl Terminal {
    /// `font` is expected to be the world's font component, so the grid and
    /// anything else drawing with the same face share one instance.
    ///
    /// `background` is the configured window colour: it becomes what
    /// [`Color::Default`] resolves to behind a cell, so the window and the text
    /// agree.
    pub(crate) fn new(font: Arc<RwLock<Font>>, background: [f32; 4]) -> Self {
        let cursor_size = font.read().unwrap().cell();

        let mut terminal = Self {
            cursor_position: (0, 0),
            cursor_size,
            cursor_visible: true,
            cursor_style: CursorStyle::default().shape(),
            size: (0, 0),
            last_texture_size: wgpu::Extent3d {
                width: 0,
                height: 0,
                depth_or_array_layers: 1,
            },
            texture: None,
            font,
            palette: Palette::new(background),
            screen: Screen::default(),
            blink: Blink::new(),
            parser: vte::Parser::new(),
        };

        // Anything the shell writes before the first frame is parsed into this
        // starting grid; `update_layout` resizes it to the window afterwards
        // without losing the text.
        terminal
            .screen
            .resize(INITIAL_COLS as usize, INITIAL_ROWS as usize);

        terminal
    }

    pub(crate) fn update_layout(&mut self, gpu: &Gpu, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }

        let (w, h) = (width as usize, height as usize);
        self.update_grid_size(w, h);
        let buffer = self.rasterize(w, h);
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let mut recreated = false;

        if self.texture.is_none() || self.last_texture_size != size {
            self.texture = Some(gpu.device().create_texture(&wgpu::TextureDescriptor {
                label: Some(WINDOW_TITLE),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            }));
            self.last_texture_size = size;
            recreated = true;
        }

        if let Some(texture) = self.texture.as_ref() {
            gpu.queue().write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &buffer,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4), // write_texture needs NO 256-byte padding
                    rows_per_image: Some(height),
                },
                size,
            );
        }

        recreated
    }

    /// Runs bytes read from the shell through the VT parser into the grid.
    ///
    /// A partial escape sequence at the end of `bytes` is remembered by the
    /// parser and completed by the next call, so callers can hand over whatever
    /// chunk size they happen to have.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.screen, bytes);
        self.update_cursor_state();
    }

    /// Refreshes everything the renderer reads about the cursor: where it is,
    /// what it looks like, and whether the blink clock has anything to animate.
    fn update_cursor_state(&mut self) {
        let style = self.screen.cursor_style;

        // The clock only runs while something changes between its two halves: a
        // cursor that is drawn and blinks, or a cell that asked for `SGR 5`.
        self.blink.active =
            (!self.screen.cursor_hidden && style.blinking()) || self.screen.blinks();

        self.cursor_position = self.screen.cursor();
        self.cursor_style = style.shape();
        self.cursor_visible = cursor_drawn(&self.blink, &self.screen);

        // The next frame paints this phase; the clock only asks for a redraw
        // when it has moved on since the last one.
        self.blink.sync();
    }

    /// Advances the blink clock, reporting whether the phase on screen changed —
    /// the one thing that needs a redraw.
    pub(crate) fn tick_blink(&mut self) -> bool {
        self.blink.tick()
    }

    /// When the blink next flips, so the event loop can sleep until then.
    /// `None` while nothing on screen blinks.
    pub(crate) fn next_blink(&self) -> Option<Instant> {
        self.blink.deadline()
    }

    /// Restarts the blink on its visible half — what typing does, so the cursor
    /// does not vanish from under the keystroke.
    pub(crate) fn wake_blink(&mut self) {
        self.blink.wake();
    }

    /// A window without focus does not blink: the cursor is left solid and a
    /// cell that asked for `SGR 5` is drawn steadily.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.blink.focused = focused;

        if focused {
            self.blink.wake();
        }
    }

    /// What the shell has asked of the keyboard, for [`encode_key`].
    pub(crate) fn key_modes(&self) -> KeyModes {
        self.screen.key_modes()
    }

    /// The answers the shell is owed for a `DSR` or a device attributes
    /// request, for the caller to write back to the pty. Taking them clears
    /// them, so every answer goes out once.
    pub(crate) fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.screen.replies)
    }

    fn update_grid_size(&mut self, width: usize, height: usize) {
        let (cell_width, cell_height) = {
            let font = self.font.read().unwrap();
            font.cell()
        };

        self.cursor_size = (cell_width, cell_height);
        self.size = (
            (width as f32 / cell_width).floor().max(1.0) as usize,
            (height as f32 / cell_height).floor().max(1.0) as usize,
        );
        self.screen.resize(self.size.0, self.size.1);
        self.update_cursor_state();
    }

    /// Paints the whole grid into an RGBA buffer, in the sRGB bytes the screen
    /// texture stores.
    ///
    /// Every cell paints its own background, blank or not, so the shader has
    /// nothing to add and an erased cell keeps the colour the shell asked for.
    /// Glyph coverage is blended between that background and the cell's
    /// foreground.
    fn rasterize(&self, w: usize, h: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; w * h * 4];
        // `DECSCNM` reverses the whole screen, so even the colour behind the
        // text is the other one.
        let reverse = self.screen.reverse;
        let background = self.palette.background(reverse);

        for pixel in buffer.chunks_exact_mut(4) {
            pixel[..3].copy_from_slice(&background);
            pixel[3] = 255;
        }

        let (cols, rows) = self.size;
        let (cell_width, cell_height) = self.cursor_size;
        if cols == 0 || rows == 0 || cell_width <= 0.0 || cell_height <= 0.0 {
            return buffer;
        }

        let font = self.font.read().unwrap();
        let Some(line) = font.line_metrics() else {
            return buffer;
        };

        let rule = ((cell_height / 12.0).round() as i32).max(1);

        // The half of the blink being painted: a `SGR 5` cell is blank on the
        // other one.
        let blink = self.blink.visible();

        // A cell's edges are rounded once and shared with its neighbour, so the
        // cells tile the texture exactly. Rounding each cell's own origin *and*
        // size instead lets the two disagree by a pixel, which shows the window
        // background as a line through every coloured region. The last edge is
        // the texture's own edge, so the grid reaches the window and no strip of
        // background is left along the right or bottom side.
        let boundary = |index: usize, cells: usize, cell: f32, extent: usize| -> i32 {
            if index == cells {
                extent as i32
            } else {
                ((index as f32 * cell).round() as i32).min(extent as i32)
            }
        };

        for row in 0..rows {
            let y = boundary(row, rows, cell_height, h);
            let height = (boundary(row + 1, rows, cell_height, h) - y).max(1);

            for col in 0..cols {
                let cell = self.screen.cell(row, col);
                let paint = self.palette.paint(cell, reverse);
                let x = boundary(col, cols, cell_width, w);
                let rect = Rect {
                    x,
                    y,
                    width: (boundary(col + 1, cols, cell_width, w) - x).max(1),
                    height,
                };

                glyphs::fill_rect(&mut buffer, w, h, rect, paint.bg);

                // A cell that asked for `SGR 5` keeps its background but loses
                // everything drawn over it on the invisible half of the blink.
                let hidden = !blink && cell.attrs.contains(Attrs::BLINK);

                // Blank cells are the common case and have nothing on top of
                // their background.
                if cell.ch != ' ' && !hidden {
                    // The characters the terminal draws itself — the box
                    // drawing, the blocks, the braille and the rest — come out
                    // of the cell's own rectangle, so that they always meet the
                    // cell next to them. Anything else is the font's to draw.
                    let drawn = {
                        let mut piece = |piece: Rect, coverage: u8| {
                            glyphs::blend_rect(
                                &mut buffer,
                                w,
                                h,
                                piece,
                                paint.bg,
                                paint.fg,
                                coverage,
                            );
                        };

                        glyphs::draw(cell.ch, rect, &mut piece)
                    };

                    if !drawn {
                        let (metrics, bitmap) = font.rasterize(
                            cell.ch,
                            cell.attrs.contains(Attrs::BOLD),
                            cell.attrs.contains(Attrs::ITALIC),
                        );

                        if metrics.width > 0 && metrics.height > 0 {
                            let baseline = row as f32 * cell_height + line.ascent;
                            let left =
                                (col as f32 * cell_width + metrics.xmin as f32).round() as i32;
                            let top = (baseline - metrics.ymin as f32 - metrics.height as f32)
                                .round() as i32;

                            for gy in 0..metrics.height {
                                let y = top + gy as i32;

                                if y < 0 || y >= h as i32 {
                                    continue;
                                }

                                for gx in 0..metrics.width {
                                    let x = left + gx as i32;

                                    if x < 0 || x >= w as i32 {
                                        continue;
                                    }

                                    let coverage = bitmap[gy * metrics.width + gx];

                                    if coverage == 0 {
                                        continue;
                                    }

                                    let pixel = (y as usize * w + x as usize) * 4;
                                    let blended = glyphs::blend(paint.bg, paint.fg, coverage);

                                    buffer[pixel..pixel + 3].copy_from_slice(&blended);
                                }
                            }
                        }
                    }
                }

                // The decorations are rules rather than glyphs: the font does
                // not have to have them, and they take the cell's own colours.
                if !hidden {
                    let mut ink = |piece: Rect, coverage: u8, color: [u8; 3]| {
                        glyphs::blend_rect(&mut buffer, w, h, piece, paint.bg, color, coverage);
                    };

                    if let Some(style) = cell.attrs.underline() {
                        let color = paint.underline;

                        underline(style, rect, rule, &mut |piece, coverage| {
                            ink(piece, coverage, color)
                        });
                    }

                    if cell.attrs.contains(Attrs::OVERLINE) {
                        ink(
                            Rect {
                                y: rect.y,
                                height: rule,
                                ..rect
                            },
                            255,
                            paint.fg,
                        );
                    }

                    if cell.attrs.contains(Attrs::STRIKE) {
                        let y = rect.y + (rect.height - rule) / 2;

                        ink(
                            Rect {
                                y,
                                height: rule,
                                ..rect
                            },
                            255,
                            paint.fg,
                        );
                    }
                }
            }
        }

        buffer
    }
}

/// Whether the cursor is painted: it has to be shown at all, and on the visible
/// half of the blink when its style is one that blinks.
fn cursor_drawn(blink: &Blink, screen: &Screen) -> bool {
    !screen.cursor_hidden && (!screen.cursor_style.blinking() || blink.visible())
}

/// The clock the cursor and every `SGR 5` cell share.
///
/// The phase is worked out from when the clock last started rather than flipped
/// by a frame, so a redraw in the middle of a half-cycle paints the same phase
/// again and the blink keeps its rate whatever the frame rate is.
#[derive(Debug)]
struct Blink {
    /// When the current half-cycle began. Restarted when the user types, so the
    /// cursor stays visible while keys are arriving.
    started: Instant,
    /// A window without focus does not blink.
    focused: bool,
    /// Whether anything changes between the two halves — a cursor that is drawn
    /// and blinks, or a cell that asked for `SGR 5`. Nothing to animate means
    /// no wake-ups at all.
    active: bool,
    /// The phase as it was last painted, so a wake-up only redraws when the
    /// screen would really change.
    shown: bool,
}

impl Blink {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            focused: true,
            active: false,
            shown: true,
        }
    }

    /// Whether the clock is running: there is focus, and something to animate.
    fn running(&self) -> bool {
        self.focused && self.active
    }

    /// Whether the visible half is showing. A stopped clock keeps it showing.
    fn visible(&self) -> bool {
        !self.running() || self.on()
    }

    /// The phase of the clock itself: on for the even half-cycles.
    fn on(&self) -> bool {
        let half_cycle = self.started.elapsed().as_nanos() / BLINK_INTERVAL.as_nanos();

        half_cycle.is_multiple_of(2)
    }

    /// When the phase next flips, or `None` while the clock is stopped.
    fn deadline(&self) -> Option<Instant> {
        if !self.running() {
            return None;
        }

        let half = BLINK_INTERVAL.as_nanos();
        let remaining = half - self.started.elapsed().as_nanos() % half;

        Some(Instant::now() + Duration::from_nanos(remaining as u64))
    }

    /// Starts a fresh visible half.
    fn wake(&mut self) {
        self.started = Instant::now();
        self.shown = true;
    }

    /// Advances to this instant, reporting whether the phase on screen changed.
    fn tick(&mut self) -> bool {
        let visible = self.visible();
        let changed = visible != self.shown;

        self.shown = visible;

        changed
    }

    /// Remembers the phase the frame about to be painted will show.
    fn sync(&mut self) {
        self.shown = self.visible();
    }
}

/// A colour a cell asks for, in the form the shell wrote it: the palette
/// decides what either of the first two means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// What `SGR` asked of a cell beyond its colours.
///
/// These are the requests, not their effect: bold is a flag even though it is
/// drawn as a brighter colour, and inverse is a flag rather than the colours
/// being swapped in place, so a later `SGR 27` can put them back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Attrs(u16);

impl Attrs {
    const BOLD: Self = Self(1 << 0);
    const DIM: Self = Self(1 << 1);
    const ITALIC: Self = Self(1 << 2);
    const UNDERLINE: Self = Self(1 << 3);
    const BLINK: Self = Self(1 << 4);
    const INVERSE: Self = Self(1 << 5);
    const HIDDEN: Self = Self(1 << 6);
    const STRIKE: Self = Self(1 << 7);
    const OVERLINE: Self = Self(1 << 8);
    /// `SGR 4:2` to `4:5`: the shape of the line under the text, kept apart
    /// from `UNDERLINE` ("there is one") so that clearing it is one bit.
    const UNDERLINE_DOUBLE: Self = Self(1 << 9);
    const UNDERLINE_CURLY: Self = Self(1 << 10);
    const UNDERLINE_DOTTED: Self = Self(1 << 11);
    const UNDERLINE_DASHED: Self = Self(1 << 12);

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    /// Every bit that describes an underline, for the sequences that clear the
    /// whole thing whatever shape it had.
    const UNDERLINES: Self = Self(
        Self::UNDERLINE.0
            | Self::UNDERLINE_DOUBLE.0
            | Self::UNDERLINE_CURLY.0
            | Self::UNDERLINE_DOTTED.0
            | Self::UNDERLINE_DASHED.0,
    );

    /// Which underline to draw under the cell, if any.
    fn underline(self) -> Option<Underline> {
        if self.contains(Self::UNDERLINE_DOUBLE) {
            Some(Underline::Double)
        } else if self.contains(Self::UNDERLINE_CURLY) {
            Some(Underline::Curly)
        } else if self.contains(Self::UNDERLINE_DOTTED) {
            Some(Underline::Dotted)
        } else if self.contains(Self::UNDERLINE_DASHED) {
            Some(Underline::Dashed)
        } else if self.contains(Self::UNDERLINE) {
            Some(Underline::Straight)
        } else {
            None
        }
    }
}

/// The shapes `SGR 4:n` can ask the underline to take. `SGR 4` alone is the
/// straight one; the curly, dotted and dashed forms are the ones kitty and
/// others added, and that Neovim's `undercurl` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Underline {
    Straight,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl std::ops::BitOr for Attrs {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// One cell of the grid: the character, how to paint it, and the attributes
/// that change the painting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    ch: char,
    fg: Color,
    bg: Color,
    /// The underline's own colour, `SGR 58`; `Color::Default` means it takes
    /// the cell's foreground, which is what almost every program means.
    underline: Color,
    attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            underline: Color::Default,
            attrs: Attrs::default(),
        }
    }
}

/// The style the next printed character gets: the one thing `SGR` changes, and
/// what `Cell` copies when a character arrives.
#[derive(Clone, Copy, Default)]
struct Pen {
    fg: Color,
    bg: Color,
    underline: Color,
    attrs: Attrs,
}

impl Pen {
    /// The cell a character printed in this style gets.
    fn cell(&self, ch: char) -> Cell {
        Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
            underline: self.underline,
            attrs: self.attrs,
        }
    }
}

/// The xterm table, which is what a shell that was offered 256 colours expects
/// to find. The dim entries double as the `SGR 1` versions of the first eight.
const ANSI: [[u8; 3]; 16] = [
    [0x28, 0x2c, 0x34],
    [0xe0, 0x6c, 0x75],
    [0x98, 0xc3, 0x79],
    [0xe5, 0xc0, 0x7b],
    [0x61, 0xaf, 0xef],
    [0xc6, 0x78, 0xdd],
    [0x56, 0xb6, 0xc2],
    [0xab, 0xb2, 0xbf],
    [0x5c, 0x63, 0x70],
    [0xbe, 0x50, 0x46],
    [0x7e, 0xc6, 0x6b],
    [0xd1, 0x9a, 0x66],
    [0x4d, 0x9b, 0xe0],
    [0xb2, 0x6c, 0xc8],
    [0x46, 0xa5, 0xb0],
    [0xf2, 0xf4, 0xf8],
];

/// The colours a cell can ask for, held in the sRGB bytes the screen texture
/// stores so that painting a cell is a copy rather than a conversion.
struct Palette {
    ansi: [[u8; 3]; 16],
    foreground: [u8; 3],
    background: [u8; 3],
}

impl Palette {
    /// `background` is the window colour the config gives, in the linear floats
    /// the shader used to work in; it is converted to sRGB once, here, so that
    /// the window and the default cell background stay the same colour.
    fn new(background: [f32; 4]) -> Self {
        Self {
            ansi: ANSI,
            foreground: [0xdc, 0xdf, 0xe4],
            background: srgb(background),
        }
    }

    /// What to paint a cell with, once the palette and the attributes that
    /// change colour have been applied.
    ///
    /// `reverse` is `DECSCNM`, the mode that draws the whole screen inverted:
    /// it swaps the two colours of every cell, which is what the mode means.
    fn paint(&self, cell: Cell, reverse: bool) -> Paint {
        let mut fg = self.resolve(cell.fg, self.foreground);
        let mut bg = self.resolve(cell.bg, self.background);

        // Bold brightens the eight base colours as well as asking for a bold
        // face, which is what the attribute has always meant.
        if let Color::Indexed(index @ 0..=7) = cell.fg
            && cell.attrs.contains(Attrs::BOLD)
        {
            fg = self.ansi[index as usize + 8];
        }

        if cell.attrs.contains(Attrs::DIM) {
            fg = fg.map(|channel| (channel as f32 * 2.0 / 3.0) as u8);
        }

        if cell.attrs.contains(Attrs::HIDDEN) {
            fg = bg;
        }

        if cell.attrs.contains(Attrs::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }

        if reverse {
            std::mem::swap(&mut fg, &mut bg);
        }

        // An underline that was not given a colour of its own follows the text
        // through all of that, so it stays visible when the cell is hidden or
        // inverted.
        let underline = match cell.underline {
            Color::Default => fg,
            color => self.resolve(color, self.foreground),
        };

        Paint { fg, bg, underline }
    }

    /// The colour behind everything, which `DECSCNM` swaps for the foreground
    /// the same way it does inside a cell.
    fn background(&self, reverse: bool) -> [u8; 3] {
        if reverse {
            self.foreground
        } else {
            self.background
        }
    }

    /// The colour a cell asked for, with `default` standing in for
    /// [`Color::Default`] — the foreground on one side of a cell, the
    /// background on the other.
    fn resolve(&self, color: Color, default: [u8; 3]) -> [u8; 3] {
        match color {
            Color::Default => default,
            Color::Indexed(index) => self.indexed(index),
            Color::Rgb(r, g, b) => [r, g, b],
        }
    }

    /// The 256-colour range: the sixteen entries above, then the 6x6x6 cube and
    /// the greys that xterm stacked on top of them.
    fn indexed(&self, index: u8) -> [u8; 3] {
        let index = index as usize;

        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let level = |stride: usize| -> u8 {
                    match (index - 16) / stride % 6 {
                        0 => 0,
                        value => (55 + 40 * value) as u8,
                    }
                };

                [level(36), level(6), level(1)]
            }
            _ => {
                let grey = (8 + 10 * (index - 232)) as u8;

                [grey, grey, grey]
            }
        }
    }
}

/// A cell ready to draw.
struct Paint {
    fg: [u8; 3],
    bg: [u8; 3],
    underline: [u8; 3],
}

/// The sRGB encoding of a linear `r, g, b` colour, in bytes.
fn srgb(color: [f32; 4]) -> [u8; 3] {
    let encode = |value: f32| -> u8 {
        let value = value.clamp(0.0, 1.0);
        let encoded = if value <= 0.003_130_8 {
            12.92 * value
        } else {
            1.055 * value.powf(1.0 / 2.4) - 0.055
        };

        (encoded * 255.0).round() as u8
    };

    [encode(color[0]), encode(color[1]), encode(color[2])]
}

/// Draws `style` as the line under a cell, along the bottom of `rect`, with
/// the ink handed to `out` a piece at a time.
///
/// The shapes are rules rather than glyphs, so the font does not have to have
/// them and they follow the cell's own underline colour. Only the curly one
/// needs a pixel at a time: it is a wave, and no rectangle is.
fn underline(style: Underline, rect: Rect, thickness: i32, out: &mut impl FnMut(Rect, u8)) {
    let rule = |y: i32| Rect { y, ..rect };

    match style {
        Underline::Straight => out(rule(rect.y + rect.height - thickness), 255),
        Underline::Double => {
            out(rule(rect.y + rect.height - thickness), 255);
            out(rule(rect.y + rect.height - thickness * 3), 255);
        }
        Underline::Dotted | Underline::Dashed => {
            // A dotted line is squares, a dashed one strokes several squares
            // long, both with a gap the same size after them.
            let unit = thickness.max(1) * if style == Underline::Dotted { 2 } else { 3 };
            let mut x = rect.x;

            while x < rect.x + rect.width {
                let width = unit.min(rect.x + rect.width - x);

                out(
                    Rect {
                        x,
                        width,
                        ..rule(rect.y + rect.height - thickness)
                    },
                    255,
                );
                x += unit * 2;
            }
        }
        Underline::Curly => {
            let y = rect.y + rect.height - thickness;
            let amplitude = (thickness * 2).max(2) as f32;
            let period = (thickness * 6).max(6) as f32;

            for x in 0..rect.width {
                let wave = (x as f32 / period * std::f32::consts::TAU).sin() * amplitude;

                out(
                    Rect {
                        x: rect.x + x,
                        y: y - amplitude as i32 + wave.round() as i32,
                        width: 1,
                        height: thickness,
                    },
                    255,
                );
            }
        }
    }
}

/// The cursor a shell asked for with `DECSCUSR` (`CSI Ps SP q`). The default is
/// the blinking block the terminal starts with; the variants match the
/// sequence's parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CursorStyle {
    #[default]
    BlinkingBlock,
    SteadyBlock,
    BlinkingUnderline,
    SteadyUnderline,
    BlinkingBar,
    SteadyBar,
}

impl CursorStyle {
    /// `DECSCUSR`'s parameter: `0` and `1` are the blinking block, and a value
    /// the terminal does not know keeps that default.
    fn from_param(param: u16) -> Self {
        match param {
            2 => Self::SteadyBlock,
            3 => Self::BlinkingUnderline,
            4 => Self::SteadyUnderline,
            5 => Self::BlinkingBar,
            6 => Self::SteadyBar,
            _ => Self::BlinkingBlock,
        }
    }

    fn blinking(self) -> bool {
        matches!(
            self,
            Self::BlinkingBlock | Self::BlinkingUnderline | Self::BlinkingBar
        )
    }

    /// The shape as the shader's `cursor_style` constant: block, bar, then
    /// underline.
    fn shape(self) -> u32 {
        match self {
            Self::BlinkingBlock | Self::SteadyBlock => 0,
            Self::BlinkingBar | Self::SteadyBar => 1,
            Self::BlinkingUnderline | Self::SteadyUnderline => 2,
        }
    }
}

/// Which of the two character sets `ESC ( 0` and friends selected, and which
/// one `SO` and `SI` switch between.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CharSet {
    #[default]
    Ascii,
    /// The DEC special graphics set, where the ASCII range `_` to `~` stands
    /// for the line-drawing characters.
    Graphics,
}

/// What `DECSC` (`ESC 7`) puts aside for `DECRC` (`ESC 8`) to bring back.
#[derive(Clone, Copy)]
struct Saved {
    cursor: (usize, usize),
    pen: Pen,
    origin: bool,
    charsets: [CharSet; 2],
    shifted_out: bool,
}

/// The character grid the VT parser draws into, plus the mode and cursor state
/// the escape sequences need.
///
/// Wide glyphs, scrollback and the alternate screen are the next things to add.
struct Screen {
    cells: Vec<Cell>,
    cols: usize,
    rows: usize,
    /// What `SGR` left behind: the style the next character is drawn with.
    pen: Pen,
    /// (column, row)
    cursor: (usize, usize),
    /// Set once the cursor reaches the right margin. The wrap happens when the
    /// *next* character arrives, which is what real terminals do and what keeps
    /// a full line from scrolling a row too early.
    wrap_pending: bool,
    /// Set by `DECTCEM` (`CSI ? 25 l`): a hidden cursor is never drawn, blinking
    /// or not.
    cursor_hidden: bool,
    /// The shape the cursor is drawn with, and whether it blinks: what
    /// `DECSCUSR` (`CSI Ps SP q`) asked for.
    cursor_style: CursorStyle,
    /// `DECSTBM` (`CSI top;bottom r`): the rows the text scrolls in, inclusive.
    /// The whole screen until a program sets them.
    top: usize,
    bottom: usize,
    /// `DECAWM` (`CSI ? 7 h`): whether a character at the right edge wraps to
    /// the next line or stays there.
    autowrap: bool,
    /// `DECOM` (`CSI ? 6 h`): whether rows are counted from the top margin.
    origin: bool,
    /// `IRM` (`CSI 4 h`): whether printing pushes the rest of the line along.
    insert: bool,
    /// `DECCKM` (`CSI ? 1 h`): whether the arrow keys send `ESC O` rather than
    /// `CSI`. The grid only records it; [`encode_key`] reads it back.
    application_keys: bool,
    /// `DECSCNM` (`CSI ? 5 h`): the whole screen drawn with foreground and
    /// background swapped.
    reverse: bool,
    /// Where `HT` stops: one flag per column, every eighth one by default.
    tabs: Vec<bool>,
    /// Where `DECSC` left the cursor.
    saved: Option<Saved>,
    /// The first and second character set, and which of them `SO` selected.
    charsets: [CharSet; 2],
    shifted_out: bool,
    /// The last character printed, for `REP` to repeat.
    last: char,
    /// The answers `DSR` and a device attributes request owe the shell.
    replies: Vec<u8>,
}

impl Default for Screen {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            cols: 0,
            rows: 0,
            pen: Pen::default(),
            cursor: (0, 0),
            wrap_pending: false,
            cursor_hidden: false,
            cursor_style: CursorStyle::default(),
            top: 0,
            bottom: 0,
            autowrap: true,
            origin: false,
            insert: false,
            application_keys: false,
            reverse: false,
            tabs: Vec::new(),
            saved: None,
            charsets: [CharSet::Ascii; 2],
            shifted_out: false,
            last: ' ',
            replies: Vec::new(),
        }
    }
}

impl Screen {
    /// A blank cell in the current style. Erasing paints the background the
    /// shell asked for, not the default one, which is what makes `SGR 41`
    /// followed by `ED` fill the screen red.
    fn blank(&self) -> Cell {
        self.pen.cell(' ')
    }

    fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    /// What the shell has asked of the keyboard, for the key encoder.
    fn key_modes(&self) -> KeyModes {
        KeyModes {
            application_cursor_keys: self.application_keys,
        }
    }

    /// Whether any cell asked for `SGR 5`, so the blink clock has something to
    /// animate even when the cursor itself does not blink.
    fn blinks(&self) -> bool {
        self.cells
            .iter()
            .any(|cell| cell.attrs.contains(Attrs::BLINK))
    }

    fn cell(&self, row: usize, col: usize) -> Cell {
        self.cells
            .get(row * self.cols + col)
            .copied()
            .unwrap_or_default()
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }

        let mut cells = vec![Cell::default(); cols * rows];

        for row in 0..rows.min(self.rows) {
            for col in 0..cols.min(self.cols) {
                cells[row * cols + col] = self.cells[row * self.cols + col];
            }
        }

        self.cells = cells;
        self.cols = cols;
        self.rows = rows;
        // The margins and the tab stops are the terminal's, not the program's,
        // so a resize puts them back the way a fresh screen has them — keeping
        // any stop that fits in the columns that are left.
        self.top = 0;
        self.bottom = rows.saturating_sub(1);

        let mut tabs = std::mem::take(&mut self.tabs);
        let known = tabs.len();
        tabs.resize(cols, false);

        for (col, stop) in tabs.iter_mut().enumerate().skip(known) {
            *stop = col.is_multiple_of(8);
        }

        self.tabs = tabs;
        self.place_absolute(self.cursor.0 as isize, self.cursor.1 as isize);
    }

    /// Whether a column has a tab stop. Every eighth one does until a program
    /// moves them with `HTS` and `TBC`.
    fn is_tab(&self, col: usize) -> bool {
        self.tabs.get(col).copied().unwrap_or(col.is_multiple_of(8))
    }

    /// Puts the cursor at `(col, row)` of the screen, clamped to the grid.
    fn place_absolute(&mut self, col: isize, row: isize) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        self.cursor = (
            col.clamp(0, self.cols as isize - 1) as usize,
            row.clamp(0, self.rows as isize - 1) as usize,
        );
        self.wrap_pending = false;
    }

    /// Puts the cursor where `CUP` and the other absolute moves ask for:
    /// counted from the top margin and confined to the scrolling region while
    /// `DECOM` is on, and from the top of the screen otherwise.
    fn place_cursor(&mut self, col: isize, row: isize) {
        let (top, bottom, offset) = if self.origin {
            (self.top as isize, self.bottom as isize, self.top as isize)
        } else {
            (0, self.rows as isize - 1, 0)
        };

        self.place_absolute(col, (row + offset).clamp(top, bottom));
    }

    /// Moves the cursor relative to where it is, as the movement escapes do:
    /// the rows stop at the scrolling region and the columns at the edges of
    /// the screen.
    fn move_cursor(&mut self, cols: isize, rows: isize) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let col = (self.cursor.0 as isize + cols).clamp(0, self.cols as isize - 1);
        let row = (self.cursor.1 as isize + rows).clamp(self.top as isize, self.bottom as isize);

        self.place_absolute(col, row);
    }

    /// `CHA` and `HPA`: the column alone, which `DECOM` does not affect.
    fn set_column(&mut self, col: isize) {
        self.place_absolute(col, self.cursor.1 as isize);
    }

    /// `VPA`: the row alone, counted from the top margin like `CUP`.
    fn set_row(&mut self, row: isize) {
        self.place_cursor(self.cursor.0 as isize, row);
    }

    fn put_char(&mut self, ch: char) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        if self.autowrap && self.wrap_pending {
            self.carriage_return();
            self.line_feed();
        }

        let (col, row) = self.cursor;

        if self.insert {
            self.insert_chars(1);
        }

        self.cells[row * self.cols + col] = self.pen.cell(ch);
        self.last = ch;

        if col + 1 == self.cols {
            self.wrap_pending = true;
        } else {
            self.cursor.0 = col + 1;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor.0 = 0;
        self.wrap_pending = false;
    }

    fn backspace(&mut self) {
        self.cursor.0 = self.cursor.0.saturating_sub(1);
        self.wrap_pending = false;
    }

    /// `HT` (a tab): the next tab stop, or the right edge when there is none.
    fn tab(&mut self) {
        if self.cols == 0 {
            return;
        }

        self.cursor.0 = ((self.cursor.0 + 1)..self.cols)
            .find(|col| self.is_tab(*col))
            .unwrap_or(self.cols - 1);
        self.wrap_pending = false;
    }

    /// `CHT` (`CSI n I`): forward `count` tab stops.
    fn forward_tabs(&mut self, count: usize) {
        for _ in 0..count {
            self.tab();
        }
    }

    /// `CBT` (`CSI n Z`): back `count` tab stops — what `Shift`+`Tab` sends.
    fn back_tabs(&mut self, count: usize) {
        if self.cols == 0 {
            return;
        }

        for _ in 0..count {
            self.cursor.0 = (0..self.cursor.0)
                .rev()
                .find(|col| self.is_tab(*col))
                .unwrap_or(0);
        }

        self.wrap_pending = false;
    }

    /// `HTS` (`ESC H`): a tab stop at the cursor.
    fn set_tab(&mut self) {
        if let Some(stop) = self.tabs.get_mut(self.cursor.0) {
            *stop = true;
        }
    }

    /// `TBC` (`CSI Ps g`): the stop at the cursor (`0`), or every one there is
    /// (`3`).
    fn clear_tabs(&mut self, mode: u16) {
        for (col, stop) in self.tabs.iter_mut().enumerate() {
            match mode {
                3 => *stop = false,
                0 if col == self.cursor.0 => *stop = false,
                _ => {}
            }
        }
    }

    fn line_feed(&mut self) {
        if self.rows == 0 {
            return;
        }

        if self.cursor.1 == self.bottom {
            self.scroll_region_up(1);
        } else if self.cursor.1 + 1 < self.rows {
            self.cursor.1 += 1;
        }

        self.wrap_pending = false;
    }

    fn reverse_index(&mut self) {
        if self.rows == 0 {
            return;
        }

        if self.cursor.1 == self.top {
            self.scroll_region_down(1);
        } else {
            self.cursor.1 = self.cursor.1.saturating_sub(1);
        }

        self.wrap_pending = false;
    }

    /// `SU`, and a line feed at the bottom margin: the text inside the margins
    /// `DECSTBM` set moves up, and blank lines in the current style come in at
    /// the bottom.
    fn scroll_region_up(&mut self, lines: usize) {
        if self.cols == 0 || self.rows == 0 || self.bottom < self.top {
            return;
        }

        let lines = lines.min(self.bottom - self.top + 1);
        let blank = self.blank();
        let (start, end) = (self.top * self.cols, (self.bottom + 1) * self.cols);

        self.cells
            .copy_within(start + lines * self.cols..end, start);
        self.cells[end - lines * self.cols..end].fill(blank);
    }

    /// `SD`, and a reverse index at the top margin: the other direction.
    fn scroll_region_down(&mut self, lines: usize) {
        if self.cols == 0 || self.rows == 0 || self.bottom < self.top {
            return;
        }

        let lines = lines.min(self.bottom - self.top + 1);
        let blank = self.blank();
        let (start, end) = (self.top * self.cols, (self.bottom + 1) * self.cols);

        self.cells
            .copy_within(start..end - lines * self.cols, start + lines * self.cols);
        self.cells[start..start + lines * self.cols].fill(blank);
    }

    fn erase_display(&mut self, mode: u16) {
        if self.cells.is_empty() {
            return;
        }

        let blank = self.blank();
        let index = self.cursor.1 * self.cols + self.cursor.0;

        match mode {
            0 => self.cells[index..].fill(blank),
            1 => self.cells[..=index].fill(blank),
            _ => self.cells.fill(blank),
        }
    }

    fn erase_line(&mut self, mode: u16) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let blank = self.blank();
        let start = self.cursor.1 * self.cols;

        match mode {
            0 => self.cells[start + self.cursor.0..start + self.cols].fill(blank),
            1 => self.cells[start..=start + self.cursor.0].fill(blank),
            _ => self.cells[start..start + self.cols].fill(blank),
        }
    }

    /// `ICH` (`CSI n @`): makes room for `count` blanks at the cursor, pushing
    /// the rest of the line right.
    fn insert_chars(&mut self, count: usize) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let blank = self.blank();
        let start = self.cursor.1 * self.cols + self.cursor.0;
        let end = (self.cursor.1 + 1) * self.cols;
        let count = count.min(end - start);

        self.cells.copy_within(start..end - count, start + count);
        self.cells[start..start + count].fill(blank);
    }

    /// `DCH` (`CSI n P`): drops `count` characters, pulling the rest of the
    /// line left.
    fn delete_chars(&mut self, count: usize) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let blank = self.blank();
        let start = self.cursor.1 * self.cols + self.cursor.0;
        let end = (self.cursor.1 + 1) * self.cols;
        let count = count.min(end - start);

        self.cells.copy_within(start + count..end, start);
        self.cells[end - count..end].fill(blank);
    }

    /// `ECH` (`CSI n X`): blanks `count` characters, leaving the rest of the
    /// line where it is.
    fn erase_chars(&mut self, count: usize) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let blank = self.blank();
        let start = self.cursor.1 * self.cols + self.cursor.0;
        let end = start + count.min(self.cols - self.cursor.0);

        self.cells[start..end].fill(blank);
    }

    /// `IL` (`CSI n L`): blank lines at the cursor, pushing the lines below
    /// them down. Like `DL`, it only does anything while the cursor is inside
    /// the scrolling region, which is what the sequence is defined to mean.
    fn insert_lines(&mut self, count: usize) {
        if !self.cursor_in_region() {
            return;
        }

        let blank = self.blank();
        let lines = count.min(self.bottom - self.cursor.1 + 1);
        let (start, end) = (self.cursor.1 * self.cols, (self.bottom + 1) * self.cols);

        self.cells
            .copy_within(start..end - lines * self.cols, start + lines * self.cols);
        self.cells[start..start + lines * self.cols].fill(blank);
    }

    /// `DL` (`CSI n M`): the same lines, dropped instead of pushed down.
    fn delete_lines(&mut self, count: usize) {
        if !self.cursor_in_region() {
            return;
        }

        let blank = self.blank();
        let lines = count.min(self.bottom - self.cursor.1 + 1);
        let (start, end) = (self.cursor.1 * self.cols, (self.bottom + 1) * self.cols);

        self.cells
            .copy_within(start + lines * self.cols..end, start);
        self.cells[end - lines * self.cols..end].fill(blank);
    }

    fn cursor_in_region(&self) -> bool {
        self.cols != 0
            && self.rows != 0
            && self.cursor.1 >= self.top
            && self.cursor.1 <= self.bottom
    }

    /// `REP` (`CSI n b`): the last character printed, `count` more times.
    fn repeat(&mut self, count: usize) {
        let last = self.last;

        for _ in 0..count {
            self.put_char(last);
        }
    }

    /// `DECSC` (`ESC 7`) and `SCOSC` (`CSI s`): the cursor and the pen — what a
    /// program that draws a prompt and then comes back needs to put back.
    fn save_cursor(&mut self) {
        self.saved = Some(Saved {
            cursor: self.cursor,
            pen: self.pen,
            origin: self.origin,
            charsets: self.charsets,
            shifted_out: self.shifted_out,
        });
    }

    /// `DECRC` (`ESC 8`) and `SCORC` (`CSI u`). With nothing saved it puts the
    /// cursor home, which is what a terminal that has just been reset does.
    fn restore_cursor(&mut self) {
        let Some(saved) = self.saved else {
            self.place_absolute(0, 0);

            return;
        };

        self.pen = saved.pen;
        self.origin = saved.origin;
        self.charsets = saved.charsets;
        self.shifted_out = saved.shifted_out;
        self.place_absolute(saved.cursor.0 as isize, saved.cursor.1 as isize);
    }

    /// `RIS` (`ESC c`): every mode, the margins and the pen back to the state a
    /// terminal starts in, and the grid blank.
    fn reset(&mut self) {
        let (cols, rows) = (self.cols, self.rows);

        *self = Screen::default();
        self.resize(cols, rows);
    }

    /// `DECSTBM` (`CSI top;bottom r`): the rows the text scrolls in. The cursor
    /// goes home afterwards, which is what puts a program's next write inside
    /// the region it just made. A region of one row, or one that ends before it
    /// starts, is ignored.
    fn set_margins(&mut self, params: &Params) {
        if self.rows == 0 {
            return;
        }

        let top = param(params, 0, 1) as usize;
        let bottom = param(params, 1, self.rows as u16) as usize;
        let top = top.saturating_sub(1);
        let bottom = bottom.saturating_sub(1).min(self.rows.saturating_sub(1));

        if top < bottom {
            self.top = top;
            self.bottom = bottom;
            self.place_absolute(0, if self.origin { self.top as isize } else { 0 });
        }
    }

    /// `DSR` (`CSI n`): `5` is the "are you there" a program sends before it
    /// decides what to draw, `6` is "where is the cursor", which a program
    /// cannot work out on its own.
    fn device_status(&mut self, params: &Params) {
        match param(params, 0, 0) {
            5 => self.replies.extend_from_slice(b"\x1b[0n"),
            6 => {
                let offset = if self.origin { self.top } else { 0 };
                let report = format!("\x1b[{};{}R", self.cursor.1 - offset + 1, self.cursor.0 + 1);

                self.replies.extend_from_slice(report.as_bytes());
            }
            _ => {}
        }
    }

    /// `DA` (`CSI c`): which terminal this is. Programs ask before they decide
    /// which escapes to use, so the answer is the conservative one — a VT100
    /// with the advanced video option — and the second request, `CSI > c`,
    /// gets the same kind of answer.
    fn device_attributes(&mut self, intermediates: &[u8]) {
        if intermediates.is_empty() {
            self.replies.extend_from_slice(b"\x1b[?1;2c");
        } else if intermediates == b">".as_slice() {
            self.replies.extend_from_slice(b"\x1b[>1;2;0c");
        }
    }

    /// `CSI Pm h` / `CSI Pm l`: the modes that are not private to DEC. Only
    /// `IRM` (4, insert mode) does anything here.
    fn set_ansi_mode(&mut self, params: &Params, enabled: bool) {
        for group in params.iter() {
            if group.first() == Some(&4) {
                self.insert = enabled;
            }
        }
    }

    /// `SGR`: changes the pen, and so everything printed after it.
    fn sgr(&mut self, params: &Params) {
        let groups: Vec<&[u16]> = params.iter().collect();

        // `CSI m` on its own is `SGR 0`.
        if groups.is_empty() {
            self.pen = Pen::default();

            return;
        }

        let mut index = 0;

        while index < groups.len() {
            let code = groups[index].first().copied().unwrap_or(0);
            index += 1;

            match code {
                0 => self.pen = Pen::default(),
                1 => self.pen.attrs.insert(Attrs::BOLD),
                2 => self.pen.attrs.insert(Attrs::DIM),
                3 => self.pen.attrs.insert(Attrs::ITALIC),
                5 => self.pen.attrs.insert(Attrs::BLINK),
                7 => self.pen.attrs.insert(Attrs::INVERSE),
                8 => self.pen.attrs.insert(Attrs::HIDDEN),
                9 => self.pen.attrs.insert(Attrs::STRIKE),
                // `22` is the only one that clears a pair.
                22 => self.pen.attrs.remove(Attrs::BOLD | Attrs::DIM),
                23 => self.pen.attrs.remove(Attrs::ITALIC),
                24 => self.set_underline(0),
                25 => self.pen.attrs.remove(Attrs::BLINK),
                27 => self.pen.attrs.remove(Attrs::INVERSE),
                28 => self.pen.attrs.remove(Attrs::HIDDEN),
                29 => self.pen.attrs.remove(Attrs::STRIKE),
                // `21` is a double underline rather than the "bold off" it
                // meant on some old terminals, which is how it is read now.
                21 => self.set_underline(2),
                53 => self.pen.attrs.insert(Attrs::OVERLINE),
                55 => self.pen.attrs.remove(Attrs::OVERLINE),
                30..=37 => self.pen.fg = Color::Indexed((code - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((code - 40) as u8),
                49 => self.pen.bg = Color::Default,
                59 => self.pen.underline = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((code - 100 + 8) as u8),
                // These four carry their value in sub-parameters: the shape of
                // the underline in `4:n`, and a colour in `38`, `48` and `58`.
                4 | 38 | 48 | 58 => index += self.extended(&groups, index - 1),
                _ => {}
            }
        }
    }

    /// `SGR 4:n` and the colour parameters: the value they carry lives in
    /// sub-parameters, either packed into the code's own group (`4:3`, the
    /// `:` form) or spread over the groups that follow (`38;5;n`). Returns how
    /// many further groups it used.
    fn extended(&mut self, groups: &[&[u16]], at: usize) -> usize {
        let group = groups[at];
        let code = group[0];

        // The `:` form keeps everything in the code's own group.
        if let [_, rest @ ..] = group
            && !rest.is_empty()
        {
            if code == 4 {
                self.set_underline(rest[0]);
            } else {
                self.set_color(code, color(rest));
            }

            return 0;
        }

        // `SGR 4` on its own is the straight underline; `4;5` is an underline
        // and a blink, so nothing that follows belongs to it.
        if code == 4 {
            self.set_underline(1);

            return 0;
        }

        // The `;` form spreads them over the following parameters.
        let value = |offset: usize| {
            groups
                .get(at + offset)
                .and_then(|group| group.first().copied())
        };

        match value(1) {
            Some(5) => {
                self.set_color(code, value(2).map(|n| Color::Indexed(n as u8)));

                2
            }
            Some(2) => {
                let channel = |offset: usize| value(offset + 2).unwrap_or(0) as u8;

                self.set_color(code, Some(Color::Rgb(channel(0), channel(1), channel(2))));

                4
            }
            _ => 0,
        }
    }

    /// `38` colours the foreground, `48` the background and `58` the underline.
    /// A colour that could not be read changes nothing.
    fn set_color(&mut self, code: u16, color: Option<Color>) {
        let Some(color) = color else {
            return;
        };

        match code {
            38 => self.pen.fg = color,
            48 => self.pen.bg = color,
            58 => self.pen.underline = color,
            _ => {}
        }
    }

    /// `SGR 4:n`: which of the underline styles the next characters get. `0`
    /// and anything unknown turn the underline off, which is also what
    /// `SGR 24` does.
    fn set_underline(&mut self, style: u16) {
        self.pen.attrs.remove(Attrs::UNDERLINES);

        let style = match style {
            1 => Attrs::UNDERLINE,
            2 => Attrs::UNDERLINE_DOUBLE,
            3 => Attrs::UNDERLINE_CURLY,
            4 => Attrs::UNDERLINE_DOTTED,
            5 => Attrs::UNDERLINE_DASHED,
            _ => return,
        };

        self.pen.attrs.insert(style);
    }

    /// `CSI ? Pm h` / `CSI ? Pm l`: the private modes the grid understands.
    /// The ones it does not — the alternate screen, mouse reporting — are
    /// consumed here so they never reach the grid as text.
    fn set_mode(&mut self, params: &Params, enabled: bool) {
        for group in params.iter() {
            match group.first() {
                // `DECCKM`: which form the arrow keys send.
                Some(1) => self.application_keys = enabled,
                // `DECSCNM`: the whole screen drawn reversed.
                Some(5) => self.reverse = enabled,
                // `DECOM`: rows counted from the top margin, which the cursor
                // goes home to rather than to the top of the screen.
                Some(6) => {
                    self.origin = enabled;
                    self.place_absolute(0, if enabled { self.top as isize } else { 0 });
                }
                // `DECAWM`: wrapping at the right edge.
                Some(7) => self.autowrap = enabled,
                // `DECTCEM`: the cursor's visibility.
                Some(25) => self.cursor_hidden = !enabled,
                _ => {}
            }
        }
    }

    /// `DECSCUSR` (`CSI Ps SP q`): the cursor's shape, and whether it blinks.
    fn set_cursor_style(&mut self, param: u16) {
        self.cursor_style = CursorStyle::from_param(param);
    }

    #[cfg(test)]
    fn line(&self, row: usize) -> String {
        (0..self.cols).map(|col| self.cell(row, col).ch).collect()
    }
}

impl Perform for Screen {
    fn print(&mut self, ch: char) {
        // The DEC special graphics set draws its line-drawing characters out of
        // the ASCII range, so a program that never leaves ASCII can still draw
        // a box — which is how every full-screen program of the 1980s did it,
        // and how some of them still do.
        let ch = match self.charsets[usize::from(self.shifted_out)] {
            CharSet::Ascii => ch,
            CharSet::Graphics => special_graphic(ch),
        };

        self.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // `ENQ`: the answer a terminal gives when a program asks whether
            // anything is there.
            0x05 => self.replies.extend_from_slice(b"\x1b[0n"),
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            0x0e => self.shifted_out = true, // `SO`: the second character set
            0x0f => self.shifted_out = false, // `SI`: back to the first
            // BEL and the rest change nothing on screen yet.
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let count = param(params, 0, 1) as isize;
        let times = count.max(1) as usize;

        match action {
            'A' => self.move_cursor(0, -count),
            'B' => self.move_cursor(0, count),
            'C' => self.move_cursor(count, 0),
            'D' => self.move_cursor(-count, 0),
            'E' => {
                self.move_cursor(0, count);
                self.carriage_return();
            }
            'F' => {
                self.move_cursor(0, -count);
                self.carriage_return();
            }
            'G' | '`' => self.set_column(param(params, 0, 1) as isize - 1),
            // `CSI H` and `CSI f` are `row;column`, the opposite order to the
            // `place_cursor(col, row)` helper.
            'H' | 'f' => self.place_cursor(
                param(params, 1, 1) as isize - 1,
                param(params, 0, 1) as isize - 1,
            ),
            'd' => self.set_row(param(params, 0, 1) as isize - 1),
            'I' => self.forward_tabs(times),
            'Z' => self.back_tabs(times),
            'b' => self.repeat(times),
            '@' => self.insert_chars(times),
            'P' => self.delete_chars(times),
            'X' => self.erase_chars(times),
            'L' => self.insert_lines(times),
            'M' => self.delete_lines(times),
            'S' => self.scroll_region_up(times),
            'T' => self.scroll_region_down(times),
            'r' => self.set_margins(params),
            'g' => self.clear_tabs(param(params, 0, 0)),
            'J' => self.erase_display(param(params, 0, 0)),
            'K' => self.erase_line(param(params, 0, 0)),
            'm' => self.sgr(params),
            'n' => self.device_status(params),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            // `DECSCUSR`: `CSI Ps SP q` picks the shape and whether it blinks.
            'q' if intermediates == b" ".as_slice() => self.set_cursor_style(param(params, 0, 1)),
            'h' | 'l' if intermediates == b"?".as_slice() => self.set_mode(params, action == 'h'),
            'h' | 'l' if intermediates.is_empty() => self.set_ansi_mode(params, action == 'h'),
            'c' => self.device_attributes(intermediates),
            // Everything else — the alternate screen, mouse reporting, the
            // window reports and the rest — is consumed here so it never
            // reaches the grid as text.
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match intermediates {
            // `ESC ( X` selects the first character set and `ESC ) X` the
            // second: `0` is the graphics one and `B` the plain ASCII one.
            [b'('] | [b')'] => {
                let charset = match byte {
                    b'0' => CharSet::Graphics,
                    _ => CharSet::Ascii,
                };

                self.charsets[usize::from(intermediates[0] == b')')] = charset;
            }
            // The escape sequences with any other intermediate — the keypad,
            // the single shifts, `ESC # 8` — change nothing here.
            [] => match byte {
                b'7' => self.save_cursor(),
                b'8' => self.restore_cursor(),
                b'D' => self.line_feed(),
                b'E' => {
                    self.carriage_return();
                    self.line_feed();
                }
                // `HTS`: a tab stop where the cursor is.
                b'H' => self.set_tab(),
                b'M' => self.reverse_index(),
                // `RIS`: everything back to how the terminal starts.
                b'c' => self.reset(),
                _ => {}
            },
            _ => {}
        }
    }
}

/// The DEC special graphics set: the ASCII range `_` to `~` stands in for the
/// line-drawing characters. Anything outside the range is unchanged, which is
/// what the set says and what keeps a program that only meant to draw a box
/// from losing its letters.
fn special_graphic(ch: char) -> char {
    match ch {
        '_' => ' ',
        '`' => '\u{25C6}', // diamond
        'a' => '\u{2592}', // checker board
        'b' => '\u{2409}', // HT
        'c' => '\u{240C}', // FF
        'd' => '\u{240D}', // CR
        'e' => '\u{240A}', // LF
        'f' => '\u{00B0}', // degree
        'g' => '\u{00B1}', // plus/minus
        'h' => '\u{2424}', // NL
        'i' => '\u{240B}', // VT
        'j' => '\u{2518}',
        'k' => '\u{2510}',
        'l' => '\u{250C}',
        'm' => '\u{2514}',
        'n' => '\u{253C}',
        'o' => '\u{23BA}',
        'p' => '\u{23BB}',
        'q' => '\u{2500}',
        'r' => '\u{23BC}',
        's' => '\u{23BD}',
        't' => '\u{251C}',
        'u' => '\u{2524}',
        'v' => '\u{2534}',
        'w' => '\u{252C}',
        'x' => '\u{2502}',
        'y' => '\u{2264}',
        'z' => '\u{2265}',
        '{' => '\u{03C0}',
        '|' => '\u{2260}',
        '}' => '\u{00A3}',
        '~' => '\u{00B7}',
        _ => ch,
    }
}

/// The colour a colour parameter carries in its sub-parameters: `5;n` for a
/// palette entry, `2;r;g;b` for a direct one, and any colour space written in
/// front of the channels ignored.
fn color(subparams: &[u16]) -> Option<Color> {
    match subparams {
        [5, index, ..] => Some(Color::Indexed(*index as u8)),
        [2, rest @ ..] if rest.len() >= 3 => {
            let channels = &rest[rest.len() - 3..];

            Some(Color::Rgb(
                channels[0] as u8,
                channels[1] as u8,
                channels[2] as u8,
            ))
        }
        _ => None,
    }
}

/// Returns parameter `index`, falling back to `default` when it was omitted.
fn param(params: &Params, index: usize, default: u16) -> u16 {
    params
        .iter()
        .nth(index)
        .and_then(|subparams| subparams.first().copied())
        .unwrap_or(default)
}

/// What the shell has asked of the keyboard, which the grid keeps and the key
/// encoder reads: the modes a program sets with `CSI ? Pm h`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyModes {
    /// `DECCKM` (`CSI ? 1 h`): the arrow and Home/End keys send `ESC O A` and
    /// friends instead of `CSI A`, which is what a full-screen program expects
    /// once it has taken the keyboard over.
    pub(crate) application_cursor_keys: bool,
}

/// Translates a key press into the bytes a shell expects on its standard input,
/// or `None` when the key means nothing to the shell.
pub(crate) fn encode_key(key: &Key, modifiers: ModifiersState, modes: KeyModes) -> Option<Vec<u8>> {
    match key {
        // Space is the one printable key winit names: X11 maps the keysym
        // straight to `NamedKey::Space`, so it never reaches the `Character`
        // arms below. What it sends is a character all the same, and `Ctrl` +
        // space is the `NUL` every terminal sends for it.
        Key::Named(NamedKey::Space) => {
            let byte = if modifiers.control_key() {
                control_code(' ')?
            } else {
                b' '
            };

            Some(with_alt(vec![byte], modifiers))
        }
        Key::Named(named) => named_key(*named, modifiers, modes),
        Key::Character(text) if modifiers.control_key() => {
            let mut bytes = Vec::with_capacity(text.len());

            for ch in text.chars() {
                bytes.push(control_code(ch)?);
            }

            Some(with_alt(bytes, modifiers))
        }
        Key::Character(text) => Some(with_alt(text.as_bytes().to_vec(), modifiers)),
        _ => None,
    }
}

fn named_key(named: NamedKey, modifiers: ModifiersState, modes: KeyModes) -> Option<Vec<u8>> {
    // The parameter a key with modifiers held sends: one plus a bit for each
    // modifier. It is xterm's encoding, and what every shell, editor and
    // readline binding in between understands.
    let parameter = 1
        + i32::from(modifiers.shift_key())
        + 2 * i32::from(modifiers.alt_key())
        + 4 * i32::from(modifiers.control_key());

    let bytes = match named {
        NamedKey::Enter => b"\r".to_vec(),
        // `Shift`+`Tab` is back-tab, which the terminal has a sequence of its
        // own for rather than a modified `Tab`.
        NamedKey::Tab if modifiers.shift_key() => b"\x1b[Z".to_vec(),
        NamedKey::Tab => b"\t".to_vec(),
        NamedKey::Backspace => b"\x7f".to_vec(),
        NamedKey::Escape => b"\x1b".to_vec(),
        NamedKey::ArrowUp => cursor(b'A', parameter, modes),
        NamedKey::ArrowDown => cursor(b'B', parameter, modes),
        NamedKey::ArrowRight => cursor(b'C', parameter, modes),
        NamedKey::ArrowLeft => cursor(b'D', parameter, modes),
        NamedKey::Home => cursor(b'H', parameter, modes),
        NamedKey::End => cursor(b'F', parameter, modes),
        NamedKey::Insert => tilde(2, parameter),
        NamedKey::Delete => tilde(3, parameter),
        NamedKey::PageUp => tilde(5, parameter),
        NamedKey::PageDown => tilde(6, parameter),
        // F1 to F4 keep the letters they were given on a VT100; the rest are
        // on the numbers xterm put them on, which the `~` form carries.
        NamedKey::F1 => function(b'P', 0, parameter),
        NamedKey::F2 => function(b'Q', 0, parameter),
        NamedKey::F3 => function(b'R', 0, parameter),
        NamedKey::F4 => function(b'S', 0, parameter),
        NamedKey::F5 => tilde(15, parameter),
        NamedKey::F6 => tilde(17, parameter),
        NamedKey::F7 => tilde(18, parameter),
        NamedKey::F8 => tilde(19, parameter),
        NamedKey::F9 => tilde(20, parameter),
        NamedKey::F10 => tilde(21, parameter),
        NamedKey::F11 => tilde(23, parameter),
        NamedKey::F12 => tilde(24, parameter),
        _ => return None,
    };

    Some(bytes)
}

/// A key whose sequence ends in a letter: `CSI A`, `CSI 1;3 A` with a modifier
/// held, or `ESC O A` in the application cursor mode `DECCKM` turns on. The
/// modified form stays in the `CSI` spelling, because the application mode has
/// no modified one.
fn cursor(final_byte: u8, parameter: i32, modes: KeyModes) -> Vec<u8> {
    if parameter > 1 {
        format!("\x1b[1;{parameter}{}", final_byte as char).into_bytes()
    } else if modes.application_cursor_keys {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// A key whose sequence ends in `~`: `CSI 5 ~`, or `CSI 5;3 ~` when a
/// modifier is held.
fn tilde(number: u8, parameter: i32) -> Vec<u8> {
    if parameter > 1 {
        format!("\x1b[{number};{parameter}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

/// A function key: `ESC O P` for the first four, which is the encoding they
/// were given on a VT100, and `CSI 1;n P` or `CSI 15 ~` when a modifier is
/// held, which is what the rest of the keys use too.
fn function(letter: u8, number: u8, parameter: i32) -> Vec<u8> {
    if number == 0 {
        if parameter > 1 {
            format!("\x1b[1;{parameter}{}", letter as char).into_bytes()
        } else {
            vec![0x1b, b'O', letter]
        }
    } else {
        tilde(number, parameter)
    }
}

/// Maps a printable character to the control code that `Ctrl` + it produces.
fn control_code(ch: char) -> Option<u8> {
    let ch = ch.to_ascii_lowercase();

    match ch {
        'a'..='z' => Some(ch as u8 - b'a' + 1),
        ' ' | '@' => Some(0x00),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

/// `Alt` + key reaches the shell as the key prefixed with `ESC`.
fn with_alt(mut bytes: Vec<u8>, modifiers: ModifiersState) -> Vec<u8> {
    if modifiers.alt_key() {
        bytes.insert(0, 0x1b);
    }

    bytes
}

#[cfg(test)]
mod tests {
    use super::{
        ANSI, Attrs, BLINK_INTERVAL, Blink, Color, CursorStyle, Font, KeyModes, Screen, Terminal,
        Underline, cursor_drawn, encode_key,
    };
    use std::sync::{Arc, RwLock};
    use std::time::Instant;
    use vte::Parser;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    /// `encode_key` with the keyboard in the state a fresh terminal is in.
    fn encode(key: &Key, modifiers: ModifiersState) -> Option<Vec<u8>> {
        encode_key(key, modifiers, KeyModes::default())
    }

    /// A terminal over the system's monospace font, as the app builds it.
    fn terminal() -> Terminal {
        let font = Font::load(16.0).expect("a system monospace font");

        Terminal::new(Arc::new(RwLock::new(font)), [0.05, 0.06, 0.08, 1.0])
    }

    /// Feeds `bytes` through a real VT parser into a fresh `cols` x `rows` grid.
    fn screen(cols: usize, rows: usize, bytes: &[u8]) -> Screen {
        let mut screen = Screen::default();
        screen.resize(cols, rows);
        Parser::new().advance(&mut screen, bytes);
        screen
    }

    #[test]
    fn text_is_written_into_the_grid_and_wraps() {
        let screen = screen(4, 2, b"abcde");

        assert_eq!(screen.line(0), "abcd");
        assert_eq!(screen.line(1), "e   ");
    }

    #[test]
    fn carriage_return_overwrites_the_line() {
        let screen = screen(5, 1, b"12345\rXY");

        assert_eq!(screen.line(0), "XY345");
    }

    #[test]
    fn lines_scroll_once_the_cursor_leaves_the_bottom() {
        let screen = screen(4, 2, b"a\r\nb\r\nc");

        assert_eq!(screen.line(0), "b   ");
        assert_eq!(screen.line(1), "c   ");
    }

    #[test]
    fn escape_sequences_are_consumed_instead_of_printed() {
        let screen = screen(11, 1, b"\x1b[01;32muser@host\x1b[00m$ ");

        assert_eq!(screen.line(0), "user@host$ ");
    }

    #[test]
    fn cursor_movement_and_erasing_are_applied() {
        let screen = screen(4, 2, b"abcd\r\nefgh\x1b[1;2H\x1b[K");

        assert_eq!(screen.cursor(), (1, 0));
        assert_eq!(screen.line(0), "a   ");
        assert_eq!(screen.line(1), "efgh");
    }

    #[test]
    fn printing_keys_are_forwarded_verbatim() {
        let bytes = encode(&Key::Character("ls".into()), ModifiersState::empty());
        assert_eq!(bytes.as_deref(), Some("ls".as_bytes()));

        let bytes = encode(&Key::Character("é".into()), ModifiersState::empty());
        assert_eq!(bytes.as_deref(), Some("é".as_bytes()));
    }

    #[test]
    fn the_space_bar_reaches_the_shell() {
        // Space is a named key on X11, not `Character(" ")`.
        assert_eq!(
            encode(&Key::Named(NamedKey::Space), ModifiersState::empty()).as_deref(),
            Some(b" ".as_slice())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Space), ModifiersState::SHIFT).as_deref(),
            Some(b" ".as_slice())
        );
        assert_eq!(
            encode(&Key::Character(" ".into()), ModifiersState::empty()).as_deref(),
            Some(b" ".as_slice())
        );
    }

    #[test]
    fn control_and_alt_space_match_what_a_shell_expects() {
        assert_eq!(
            encode(&Key::Named(NamedKey::Space), ModifiersState::CONTROL).as_deref(),
            Some(b"\x00".as_slice())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Space), ModifiersState::ALT).as_deref(),
            Some(b"\x1b ".as_slice())
        );
    }

    #[test]
    fn control_and_special_keys_match_what_a_shell_expects() {
        assert_eq!(
            encode(&Key::Character("c".into()), ModifiersState::CONTROL).as_deref(),
            Some(b"\x03".as_slice())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Enter), ModifiersState::empty()).as_deref(),
            Some(b"\r".as_slice())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), ModifiersState::empty()).as_deref(),
            Some(b"\x7f".as_slice())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowUp), ModifiersState::empty()).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            encode(&Key::Character("f".into()), ModifiersState::ALT).as_deref(),
            Some(b"\x1bf".as_slice())
        );
    }

    #[test]
    fn the_function_keys_send_the_sequences_a_shell_expects() {
        let plain = |named| encode(&Key::Named(named), ModifiersState::empty());

        // `F1` to `F4` keep the letters they were given on a VT100.
        assert_eq!(plain(NamedKey::F1).as_deref(), Some(b"\x1bOP".as_slice()));
        assert_eq!(plain(NamedKey::F4).as_deref(), Some(b"\x1bOS".as_slice()));
        // The rest are on the numbers xterm put them on.
        assert_eq!(plain(NamedKey::F5).as_deref(), Some(b"\x1b[15~".as_slice()));
        assert_eq!(plain(NamedKey::F6).as_deref(), Some(b"\x1b[17~".as_slice()));
        assert_eq!(
            plain(NamedKey::F12).as_deref(),
            Some(b"\x1b[24~".as_slice())
        );
    }

    #[test]
    fn a_modified_key_carries_the_modifier_in_its_parameters() {
        // `Shift`+`Tab` is back-tab, which has a sequence of its own.
        let back_tab = encode(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT);
        assert_eq!(back_tab.as_deref(), Some(b"\x1b[Z".as_slice()));

        // The rest add xterm's parameter: one plus a bit per modifier.
        let control_right = encode(&Key::Named(NamedKey::ArrowRight), ModifiersState::CONTROL);
        assert_eq!(control_right.as_deref(), Some(b"\x1b[1;5C".as_slice()));

        let alt_f5 = encode(&Key::Named(NamedKey::F5), ModifiersState::ALT);
        assert_eq!(alt_f5.as_deref(), Some(b"\x1b[15;3~".as_slice()));

        let shift_f1 = encode(&Key::Named(NamedKey::F1), ModifiersState::SHIFT);
        assert_eq!(shift_f1.as_deref(), Some(b"\x1b[1;2P".as_slice()));
    }

    #[test]
    fn the_application_cursor_mode_changes_the_arrow_keys() {
        let modes = screen(1, 1, b"\x1b[?1h").key_modes();

        assert!(modes.application_cursor_keys);
        assert_eq!(
            encode_key(
                &Key::Named(NamedKey::ArrowUp),
                ModifiersState::empty(),
                modes
            )
            .as_deref(),
            Some(b"\x1bOA".as_slice())
        );
        assert!(
            !screen(1, 1, b"\x1b[?1h\x1b[?1l")
                .key_modes()
                .application_cursor_keys
        );
    }

    #[test]
    fn configured_terminal_draws_what_the_shell_wrote() {
        let mut terminal = terminal();
        terminal.update_grid_size(256, 64);
        terminal.feed(b"A");

        let pixels = terminal.rasterize(256, 64);

        assert!(pixels.chunks_exact(4).any(|pixel| pixel[0] != 0));
    }

    #[test]
    fn sgr_leaves_the_pen_the_following_cells_are_drawn_with() {
        let screen = screen(2, 1, b"\x1b[1;31;44mX\x1b[0mY");

        assert_eq!(screen.cell(0, 0).fg, Color::Indexed(1));
        assert_eq!(screen.cell(0, 0).bg, Color::Indexed(4));
        assert!(screen.cell(0, 0).attrs.contains(Attrs::BOLD));

        assert_eq!(screen.cell(0, 1).fg, Color::Default);
        assert!(!screen.cell(0, 1).attrs.contains(Attrs::BOLD));
    }

    #[test]
    fn bright_and_extended_colours_are_parsed() {
        let screen = screen(2, 1, b"\x1b[91mX\x1b[38;5;196mY");

        assert_eq!(screen.cell(0, 0).fg, Color::Indexed(9));
        assert_eq!(screen.cell(0, 1).fg, Color::Indexed(196));
    }

    #[test]
    fn a_true_colour_and_the_colon_form_agree() {
        let screen = screen(2, 1, b"\x1b[38;2;10;20;30mX\x1b[48:2:1:2:3mY");

        assert_eq!(screen.cell(0, 0).fg, Color::Rgb(10, 20, 30));
        assert_eq!(screen.cell(0, 1).bg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn erasing_keeps_the_background_the_shell_asked_for() {
        let screen = screen(4, 1, b"\x1b[41m\x1b[K");

        for col in 0..4 {
            assert_eq!(screen.cell(0, col).bg, Color::Indexed(1));
        }
    }

    #[test]
    fn the_default_background_reaches_the_pixels() {
        let mut terminal = terminal();
        terminal.update_grid_size(64, 32);

        let pixels = terminal.rasterize(64, 32);

        // What `0.05, 0.06, 0.08` encoded to when the shader added it by hand.
        assert_eq!(&pixels[0..3], &[63, 69, 80]);
    }

    #[test]
    fn a_coloured_background_reaches_the_pixels() {
        let mut terminal = terminal();
        terminal.update_grid_size(64, 32);
        terminal.feed(b"\x1b[41m ");

        let pixels = terminal.rasterize(64, 32);

        assert_eq!(&pixels[0..3], &ANSI[1]);
    }

    #[test]
    fn a_coloured_grid_has_no_background_showing_between_cells() {
        let mut terminal = terminal();
        // A window whose rows do not divide the height evenly is exactly the
        // case that used to leave a 1px seam between rows.
        let (w, h) = (256, 300);
        terminal.update_grid_size(w, h);
        terminal.feed(b"\x1b[41m\x1b[2J");

        let pixels = terminal.rasterize(w, h);
        let default_bg = [63, 69, 80];

        let seams = pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] == default_bg)
            .count();

        assert_eq!(seams, 0);
    }

    #[test]
    fn resizing_rebuilds_the_grid_without_losing_the_text() {
        let mut terminal = terminal();
        terminal.update_grid_size(1024, 640);
        terminal.feed(b"ab\tc\r\nX");

        let large_grid = terminal.size;
        terminal.update_grid_size(512, 320);

        assert!(terminal.size.0 < large_grid.0);
        assert!(terminal.size.1 < large_grid.1);
        assert_eq!(terminal.screen.line(0).trim_end(), "ab      c");
        assert_eq!(terminal.screen.line(1).trim_end(), "X");
    }

    #[test]
    fn the_blink_attribute_is_set_and_cleared_by_sgr() {
        let screen = screen(2, 1, b"\x1b[5mX\x1b[25mY");

        assert!(screen.cell(0, 0).attrs.contains(Attrs::BLINK));
        assert!(!screen.cell(0, 1).attrs.contains(Attrs::BLINK));
    }

    #[test]
    fn decscusr_picks_the_cursor_shape_and_whether_it_blinks() {
        assert_eq!(
            screen(1, 1, b"\x1b[5 q").cursor_style,
            CursorStyle::BlinkingBar
        );
        assert_eq!(
            screen(1, 1, b"\x1b[2 q").cursor_style,
            CursorStyle::SteadyBlock
        );
        // No parameter is the blinking block the terminal starts with.
        assert_eq!(
            screen(1, 1, b"\x1b[ q").cursor_style,
            CursorStyle::default()
        );
    }

    #[test]
    fn dectcem_hides_the_cursor_until_it_is_shown_again() {
        assert!(screen(1, 1, b"\x1b[?25l").cursor_hidden);
        assert!(!screen(1, 1, b"\x1b[?25l\x1b[?25h").cursor_hidden);
        assert!(!screen(1, 1, b"\x1b[?25h").cursor_hidden);
    }

    #[test]
    fn vims_modes_get_the_cursor_shape_they_ask_for() {
        // What Vim and Neovim send for `guicursor`'s defaults
        // (`n-v-c-sm:block,i-ci-ve:ver25,r-cr-o:hor20`): a block in normal mode,
        // the thin bar that `ver25` stands for in insert mode, and an underline
        // for replace mode's `hor20`.
        let shape = |bytes: &[u8]| screen(1, 1, bytes).cursor_style;

        assert_eq!(shape(b"\x1b[2 q").shape(), 0); // block
        assert_eq!(shape(b"\x1b[6 q").shape(), 1); // bar, thinner than the block
        assert_eq!(shape(b"\x1b[4 q").shape(), 2); // underline

        // Every mode is steady: the cursor must not blink out while one lasts.
        for bytes in [b"\x1b[2 q", b"\x1b[6 q", b"\x1b[4 q"] {
            assert!(!shape(bytes).blinking());
        }
    }

    #[test]
    fn the_blink_clock_alternates_on_its_interval() {
        let mut blink = Blink::new();

        // Nothing to animate: it shows, and it never has to wake the loop.
        assert!(blink.visible());
        assert_eq!(blink.deadline(), None);

        blink.active = true;
        assert!(blink.visible()); // a fresh clock starts on the visible half
        assert!(blink.deadline().is_some());

        blink.started = Instant::now() - BLINK_INTERVAL;
        assert!(!blink.visible());

        blink.started = Instant::now() - BLINK_INTERVAL * 2;
        assert!(blink.visible());
    }

    #[test]
    fn a_window_without_focus_does_not_blink() {
        let mut blink = Blink::new();
        blink.active = true;
        blink.focused = false;
        blink.started = Instant::now() - BLINK_INTERVAL;

        assert!(blink.visible());
        assert_eq!(blink.deadline(), None);
    }

    #[test]
    fn typing_starts_a_fresh_visible_half() {
        let mut blink = Blink::new();
        blink.active = true;
        blink.started = Instant::now() - BLINK_INTERVAL;
        assert!(!blink.visible());

        blink.wake();

        assert!(blink.visible());
    }

    #[test]
    fn the_cursor_is_drawn_only_when_it_is_shown_and_on_the_visible_half() {
        let mut blink = Blink::new();
        let mut screen = Screen::default();

        assert!(cursor_drawn(&blink, &screen));

        // A steady style keeps the cursor drawn even on the clock's off half.
        screen.cursor_style = CursorStyle::SteadyBlock;
        blink.active = true;
        blink.started = Instant::now() - BLINK_INTERVAL;
        assert!(!blink.visible());
        assert!(cursor_drawn(&blink, &screen));

        // A cursor the shell hid is not drawn at all.
        screen.cursor_hidden = true;
        assert!(!cursor_drawn(&blink, &screen));
    }

    #[test]
    fn a_blinking_cell_is_blank_on_the_off_half() {
        let mut terminal = terminal();
        terminal.update_grid_size(256, 64);
        terminal.feed(b"\x1b[5mX");

        // Anything that is not the default background is a painted pixel.
        let painted = |pixels: &[u8]| pixels.chunks_exact(4).filter(|p| p[0] != 63).count();

        let on = painted(&terminal.rasterize(256, 64));

        terminal.blink.started = Instant::now() - BLINK_INTERVAL;
        let off = painted(&terminal.rasterize(256, 64));

        assert!(on > 0);
        assert_eq!(off, 0);
    }

    #[test]
    fn the_underline_styles_come_from_sgr_four() {
        let screen = screen(4, 1, b"\x1b[4mX\x1b[4:3mY\x1b[24mZ\x1b[21mW");

        assert_eq!(
            screen.cell(0, 0).attrs.underline(),
            Some(Underline::Straight)
        );
        // `4:3` is the curly underline, and `24` clears whichever one was set.
        assert_eq!(screen.cell(0, 1).attrs.underline(), Some(Underline::Curly));
        assert_eq!(screen.cell(0, 2).attrs.underline(), None);
        assert_eq!(screen.cell(0, 3).attrs.underline(), Some(Underline::Double));
    }

    #[test]
    fn overline_and_an_underline_colour_are_parsed() {
        let screen = screen(2, 1, b"\x1b[53;58;5;1mX\x1b[55;59mY");

        assert!(screen.cell(0, 0).attrs.contains(Attrs::OVERLINE));
        assert_eq!(screen.cell(0, 0).underline, Color::Indexed(1));
        assert!(!screen.cell(0, 1).attrs.contains(Attrs::OVERLINE));
        assert_eq!(screen.cell(0, 1).underline, Color::Default);
    }

    #[test]
    fn the_margins_are_the_rows_the_text_scrolls_in() {
        // `DECSTBM` sets the region, and text scrolled off the bottom of it
        // leaves the rows outside alone.
        let screen = screen(3, 4, b"\x1b[2;3r\x1b[3;1Ha\r\nb\r\nc");

        assert_eq!(screen.line(0), "   ");
        assert_eq!(screen.line(1), "b  ");
        assert_eq!(screen.line(2), "c  ");
        assert_eq!(screen.line(3), "   ");
    }

    #[test]
    fn origin_mode_counts_rows_from_the_top_margin() {
        let screen = screen(3, 4, b"\x1b[2;4r\x1b[?6h\x1b[1;1HX");

        assert_eq!(screen.line(1), "X  ");
        assert_eq!(screen.cursor(), (1, 1));
    }

    #[test]
    fn wrapping_can_be_turned_off() {
        // With `DECAWM` off the text stays on the one line, overwriting the
        // last column, which is what a program that wraps its own text wants.
        let screen = screen(3, 2, b"\x1b[?7labcdef");

        assert_eq!(screen.line(0), "abf");
        assert_eq!(screen.line(1), "   ");
    }

    #[test]
    fn the_cursor_and_the_pen_come_back_where_they_were_saved() {
        let screen = screen(5, 2, b"\x1b[1;31m\x1b7\x1b[2;3H\x1b8X");

        assert_eq!(screen.cell(0, 0).ch, 'X');
        assert_eq!(screen.cell(0, 0).fg, Color::Indexed(1));
        assert_eq!(screen.cursor(), (1, 0));
    }

    #[test]
    fn inserting_and_deleting_characters_shifts_the_rest_of_the_line() {
        // `@` makes room, `P` takes it away again and `X` blanks in place.
        assert_eq!(screen(5, 1, b"abcde\x1b[1;2H\x1b[3@").line(0), "a   b");
        assert_eq!(screen(5, 1, b"abcde\x1b[1;2H\x1b[2P").line(0), "ade  ");
        assert_eq!(screen(5, 1, b"abcde\x1b[1;2H\x1b[2X").line(0), "a  de");

        // `L` opens a blank line at the cursor and pushes the rest down.
        let screen = screen(2, 3, b"ab\r\ncd\x1b[1;1H\x1b[L");

        assert_eq!(screen.line(0), "  ");
        assert_eq!(screen.line(1), "ab");
        assert_eq!(screen.line(2), "cd");
    }

    #[test]
    fn tab_stops_can_be_set_and_cleared() {
        // `HTS` puts a stop where the cursor is, so the next tab lands on it.
        assert_eq!(
            screen(12, 1, b"\x1b[1;3H\x1bH\x1b[H\tX").line(0),
            "  X         "
        );

        // `TBC 3` clears them all, and a tab with nowhere to go stops at the
        // right edge.
        assert_eq!(screen(12, 1, b"\x1b[3g\tX").line(0), "           X");

        // `CHT` walks forward over them and `CBT` — what `Shift`+`Tab` sends —
        // walks back, so the two `x`s land eight columns apart.
        assert_eq!(
            screen(24, 1, b"\x1b[2Ix").line(0).trim_end(),
            "                x"
        );
        assert_eq!(
            screen(24, 1, b"\x1b[2I\x1b[Zx").line(0).trim_end(),
            "        x"
        );
    }

    #[test]
    fn the_terminal_answers_a_program_that_asks_about_it() {
        // `DA`: what kind of terminal this is.
        assert_eq!(screen(4, 2, b"\x1b[c").replies.as_slice(), b"\x1b[?1;2c");

        // `DSR 5`: whether anything is there at all.
        assert_eq!(screen(4, 2, b"\x1b[5n").replies.as_slice(), b"\x1b[0n");

        // `DSR 6`: where the cursor is, which a program cannot work out alone.
        assert_eq!(
            screen(4, 2, b"\x1b[2;3H\x1b[6n").replies.as_slice(),
            b"\x1b[2;3R"
        );
    }

    #[test]
    fn the_special_graphics_set_draws_a_box_out_of_ascii() {
        // `ESC ( 0` selects the DEC special graphics set, where `lqk` is a
        // top-left corner, a horizontal line and a top-right one; `ESC ( B`
        // gives the letters back.
        assert_eq!(screen(4, 1, b"\x1b(0lqk\x1b(Ba").line(0), "┌─┐a");
    }

    #[test]
    fn a_block_element_is_drawn_from_the_cell_it_is_in() {
        let mut terminal = terminal();
        terminal.update_grid_size(64, 32);
        terminal.feed("\x1b[31m█".as_bytes());

        let pixels = terminal.rasterize(64, 32);
        // The block is the cell: its first pixel and the last one of the first
        // cell both take the foreground colour.
        let last = (terminal.cursor_size.0 as usize - 1) * 4;

        assert_eq!(&pixels[0..3], &ANSI[1]);
        assert_eq!(&pixels[last..last + 3], &ANSI[1]);
    }

    #[test]
    fn reverse_video_swaps_the_screen() {
        let mut terminal = terminal();
        terminal.update_grid_size(64, 32);
        terminal.feed(b"\x1b[?5h");

        let pixels = terminal.rasterize(64, 32);

        // The window colour now stands in for the text colour: the screen is
        // drawn the other way round.
        assert_eq!(&pixels[0..3], &[0xdc, 0xdf, 0xe4]);
    }
}
