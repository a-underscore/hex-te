use crate::font::Font;
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

        for pixel in buffer.chunks_exact_mut(4) {
            pixel[..3].copy_from_slice(&self.palette.background);
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
                let paint = self.palette.paint(cell);
                let x = boundary(col, cols, cell_width, w);
                let rect = Rect {
                    x,
                    y,
                    width: (boundary(col + 1, cols, cell_width, w) - x).max(1),
                    height,
                };

                fill_rect(&mut buffer, w, h, rect, paint.bg);

                // A cell that asked for `SGR 5` keeps its background but loses
                // everything drawn over it on the invisible half of the blink.
                let hidden = !blink && cell.attrs.contains(Attrs::BLINK);

                // Blank cells are the common case and have nothing on top of
                // their background.
                if cell.ch != ' ' && !hidden {
                    let (metrics, bitmap) = font.rasterize(cell.ch);

                    if metrics.width > 0 && metrics.height > 0 {
                        let baseline = row as f32 * cell_height + line.ascent;
                        let left = (col as f32 * cell_width + metrics.xmin as f32).round() as i32;
                        let top =
                            (baseline - metrics.ymin as f32 - metrics.height as f32).round() as i32;

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
                                let blended = blend(paint.bg, paint.fg, coverage);

                                buffer[pixel..pixel + 3].copy_from_slice(&blended);
                            }
                        }
                    }
                }

                // Underline and strikethrough are rules, not glyphs.
                if cell.attrs.contains(Attrs::UNDERLINE) && !hidden {
                    let y = rect.y + rect.height - rule;

                    fill_rect(
                        &mut buffer,
                        w,
                        h,
                        Rect {
                            y,
                            height: rule,
                            ..rect
                        },
                        paint.fg,
                    );
                }

                if cell.attrs.contains(Attrs::STRIKE) && !hidden {
                    let y = rect.y + (rect.height - rule) / 2;

                    fill_rect(
                        &mut buffer,
                        w,
                        h,
                        Rect {
                            y,
                            height: rule,
                            ..rect
                        },
                        paint.fg,
                    );
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

    fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }
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
    attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
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
    attrs: Attrs,
}

impl Pen {
    /// The cell a character printed in this style gets.
    fn cell(&self, ch: char) -> Cell {
        Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
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
    fn paint(&self, cell: Cell) -> Paint {
        let mut fg = self.resolve(cell.fg, self.foreground);
        let mut bg = self.resolve(cell.bg, self.background);

        // Bold brightens the eight base colours instead of asking for a bold
        // face, which there is only one of.
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

        Paint { fg, bg }
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

/// A rectangle of the screen texture, in pixels.
#[derive(Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

/// Fills `rect` with `color`, clipped to the buffer.
fn fill_rect(buffer: &mut [u8], w: usize, h: usize, rect: Rect, color: [u8; 3]) {
    let left = rect.x.clamp(0, w as i32);
    let right = (rect.x + rect.width).clamp(0, w as i32);
    let top = rect.y.clamp(0, h as i32);
    let bottom = (rect.y + rect.height).clamp(0, h as i32);

    for y in top..bottom {
        for x in left..right {
            let pixel = (y as usize * w + x as usize) * 4;

            buffer[pixel..pixel + 3].copy_from_slice(&color);
        }
    }
}

/// `fg` over `bg` by `coverage`, in the sRGB bytes the texture holds. Blending
/// there rather than in linear light is what the 8-bit texture does anyway, and
/// what every terminal that draws text this way does.
fn blend(bg: [u8; 3], fg: [u8; 3], coverage: u8) -> [u8; 3] {
    let mix = |bg: u8, fg: u8| -> u8 {
        let blended = bg as i32 + (fg as i32 - bg as i32) * coverage as i32 / 255;

        blended.clamp(0, 255) as u8
    };

    [mix(bg[0], fg[0]), mix(bg[1], fg[1]), mix(bg[2], fg[2])]
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

/// The character grid the VT parser draws into, plus the little state it needs
/// (the pen, the cursor and a pending wrap).
///
/// Wide glyphs, scrollback and the alternate screen are the next things to add.
#[derive(Default)]
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
        self.place_cursor(self.cursor.0 as isize, self.cursor.1 as isize);
    }

    /// Puts the cursor at `(col, row)`, clamped to the grid.
    fn place_cursor(&mut self, col: isize, row: isize) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        self.cursor = (
            col.clamp(0, self.cols as isize - 1) as usize,
            row.clamp(0, self.rows as isize - 1) as usize,
        );
        self.wrap_pending = false;
    }

    fn move_cursor(&mut self, cols: isize, rows: isize) {
        self.place_cursor(self.cursor.0 as isize + cols, self.cursor.1 as isize + rows);
    }

    fn put_char(&mut self, ch: char) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        if self.wrap_pending {
            self.carriage_return();
            self.line_feed();
        }

        let (col, row) = self.cursor;
        self.cells[row * self.cols + col] = self.pen.cell(ch);

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

    fn tab(&mut self) {
        if self.cols == 0 {
            return;
        }

        self.cursor.0 = ((self.cursor.0 / 8 + 1) * 8).min(self.cols - 1);
        self.wrap_pending = false;
    }

    fn line_feed(&mut self) {
        if self.rows == 0 {
            return;
        }

        if self.cursor.1 + 1 == self.rows {
            self.scroll_up();
        } else {
            self.cursor.1 += 1;
        }

        self.wrap_pending = false;
    }

    fn reverse_index(&mut self) {
        if self.rows == 0 {
            return;
        }

        if self.cursor.1 == 0 {
            self.scroll_down();
        } else {
            self.cursor.1 -= 1;
        }

        self.wrap_pending = false;
    }

    fn scroll_up(&mut self) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let blank = self.blank();

        if self.rows == 1 {
            self.cells.fill(blank);
            return;
        }

        self.cells.drain(..self.cols);
        self.cells.resize(self.cols * self.rows, blank);
    }

    fn scroll_down(&mut self) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let blank = self.blank();

        if self.rows == 1 {
            self.cells.fill(blank);
            return;
        }

        self.cells.truncate(self.cols * (self.rows - 1));
        self.cells.resize(self.cols * self.rows, blank);
        self.cells.rotate_right(self.cols);
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
                4 => self.pen.attrs.insert(Attrs::UNDERLINE),
                5 => self.pen.attrs.insert(Attrs::BLINK),
                7 => self.pen.attrs.insert(Attrs::INVERSE),
                8 => self.pen.attrs.insert(Attrs::HIDDEN),
                9 => self.pen.attrs.insert(Attrs::STRIKE),
                // `22` is the only one that clears a pair.
                22 => self.pen.attrs.remove(Attrs::BOLD | Attrs::DIM),
                23 => self.pen.attrs.remove(Attrs::ITALIC),
                24 => self.pen.attrs.remove(Attrs::UNDERLINE),
                25 => self.pen.attrs.remove(Attrs::BLINK),
                27 => self.pen.attrs.remove(Attrs::INVERSE),
                28 => self.pen.attrs.remove(Attrs::HIDDEN),
                29 => self.pen.attrs.remove(Attrs::STRIKE),
                30..=37 => self.pen.fg = Color::Indexed((code - 30) as u8),
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((code - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((code - 100 + 8) as u8),
                // `38`/`48` carry their colour in the parameters that follow.
                38 | 48 => index += self.extended(&groups, index - 1),
                _ => {}
            }
        }
    }

    /// `SGR 38`/`48`: a colour written as `38;5;n`, `38;2;r;g;b`, or the same
    /// packed into the one `:` group. Returns how many further parameters it
    /// used.
    fn extended(&mut self, groups: &[&[u16]], at: usize) -> usize {
        let group = groups[at];

        // The `:` form keeps the whole colour in the code's own group, with the
        // colour space — if the writer gave one — sitting before the channels.
        if let [code, marker, rest @ ..] = group {
            let color = match marker {
                5 => rest.first().map(|&n| Color::Indexed(n as u8)),
                2 if rest.len() >= 3 => {
                    let channels = &rest[rest.len() - 3..];

                    Some(Color::Rgb(
                        channels[0] as u8,
                        channels[1] as u8,
                        channels[2] as u8,
                    ))
                }
                _ => None,
            };

            self.set_color(*code, color);

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
                self.set_color(group[0], value(2).map(|n| Color::Indexed(n as u8)));

                2
            }
            Some(2) => {
                let channel = |offset: usize| value(offset + 2).unwrap_or(0) as u8;

                self.set_color(
                    group[0],
                    Some(Color::Rgb(channel(0), channel(1), channel(2))),
                );

                4
            }
            _ => 0,
        }
    }

    /// `38` colours the foreground, `48` the background. A colour that could
    /// not be read changes nothing.
    fn set_color(&mut self, code: u16, color: Option<Color>) {
        let Some(color) = color else {
            return;
        };

        match code {
            38 => self.pen.fg = color,
            48 => self.pen.bg = color,
            _ => {}
        }
    }

    /// `CSI ? Pm h` / `CSI ? Pm l`: the private modes the grid understands.
    /// Only `DECTCEM` (25, the cursor's visibility) is honoured; the rest are
    /// consumed here so they never reach the grid as text.
    fn set_mode(&mut self, params: &Params, enabled: bool) {
        for group in params.iter() {
            if group.first() == Some(&25) {
                self.cursor_hidden = !enabled;
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
        self.put_char(ch);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x08 => self.backspace(),
            0x09 => self.tab(),
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            // BEL, SO, SI and friends change nothing on screen yet.
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let count = param(params, 0, 1) as isize;

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
            'G' => self.place_cursor(param(params, 0, 1) as isize - 1, self.cursor.1 as isize),
            // `CSI H` and `CSI f` are `row;column`, the opposite order to the
            // `place_cursor(col, row)` helper.
            'H' | 'f' => self.place_cursor(
                param(params, 1, 1) as isize - 1,
                param(params, 0, 1) as isize - 1,
            ),
            'd' => self.place_cursor(self.cursor.0 as isize, param(params, 0, 1) as isize - 1),
            'J' => self.erase_display(param(params, 0, 0)),
            'K' => self.erase_line(param(params, 0, 0)),
            'm' => self.sgr(params),
            // `DECTCEM`: `CSI ? 25 h` shows the cursor and `CSI ? 25 l` hides it.
            'h' | 'l' if intermediates == b"?".as_slice() => self.set_mode(params, action == 'h'),
            // `DECSCUSR`: `CSI Ps SP q` picks the shape and whether it blinks.
            'q' if intermediates == b" ".as_slice() => self.set_cursor_style(param(params, 0, 1)),
            // Everything else — the alternate screen, device status and the rest
            // of the modes — is consumed here so it never reaches the grid as
            // text.
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }

        match byte {
            b'D' => self.line_feed(),
            b'E' => {
                self.carriage_return();
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            _ => {}
        }
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

/// Translates a key press into the bytes a shell expects on its standard input,
/// or `None` when the key means nothing to the shell.
pub(crate) fn encode_key(key: &Key, modifiers: ModifiersState) -> Option<Vec<u8>> {
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
        Key::Named(named) => named_key(*named),
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

fn named_key(named: NamedKey) -> Option<Vec<u8>> {
    let bytes = match named {
        NamedKey::Enter => b"\r".to_vec(),
        NamedKey::Tab => b"\t".to_vec(),
        NamedKey::Backspace => b"\x7f".to_vec(),
        NamedKey::Escape => b"\x1b".to_vec(),
        NamedKey::ArrowUp => b"\x1b[A".to_vec(),
        NamedKey::ArrowDown => b"\x1b[B".to_vec(),
        NamedKey::ArrowRight => b"\x1b[C".to_vec(),
        NamedKey::ArrowLeft => b"\x1b[D".to_vec(),
        NamedKey::Home => b"\x1b[H".to_vec(),
        NamedKey::End => b"\x1b[F".to_vec(),
        NamedKey::Insert => b"\x1b[2~".to_vec(),
        NamedKey::Delete => b"\x1b[3~".to_vec(),
        NamedKey::PageUp => b"\x1b[5~".to_vec(),
        NamedKey::PageDown => b"\x1b[6~".to_vec(),
        _ => return None,
    };

    Some(bytes)
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
        ANSI, Attrs, BLINK_INTERVAL, Blink, Color, CursorStyle, Font, Screen, Terminal,
        cursor_drawn, encode_key,
    };
    use std::sync::{Arc, RwLock};
    use std::time::Instant;
    use vte::Parser;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

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
        let bytes = encode_key(&Key::Character("ls".into()), ModifiersState::empty());
        assert_eq!(bytes.as_deref(), Some("ls".as_bytes()));

        let bytes = encode_key(&Key::Character("é".into()), ModifiersState::empty());
        assert_eq!(bytes.as_deref(), Some("é".as_bytes()));
    }

    #[test]
    fn the_space_bar_reaches_the_shell() {
        // Space is a named key on X11, not `Character(" ")`.
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), ModifiersState::empty()).as_deref(),
            Some(b" ".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), ModifiersState::SHIFT).as_deref(),
            Some(b" ".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Character(" ".into()), ModifiersState::empty()).as_deref(),
            Some(b" ".as_slice())
        );
    }

    #[test]
    fn control_and_alt_space_match_what_a_shell_expects() {
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), ModifiersState::CONTROL).as_deref(),
            Some(b"\x00".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Space), ModifiersState::ALT).as_deref(),
            Some(b"\x1b ".as_slice())
        );
    }

    #[test]
    fn control_and_special_keys_match_what_a_shell_expects() {
        assert_eq!(
            encode_key(&Key::Character("c".into()), ModifiersState::CONTROL).as_deref(),
            Some(b"\x03".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Enter), ModifiersState::empty()).as_deref(),
            Some(b"\r".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::Backspace), ModifiersState::empty()).as_deref(),
            Some(b"\x7f".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::ArrowUp), ModifiersState::empty()).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Character("f".into()), ModifiersState::ALT).as_deref(),
            Some(b"\x1bf".as_slice())
        );
        assert_eq!(
            encode_key(&Key::Named(NamedKey::F5), ModifiersState::empty()),
            None
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
}
