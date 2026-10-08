use crate::pty::{INITIAL_COLS, INITIAL_ROWS};
use crate::{WINDOW_TITLE, gpu::Gpu};
use fontdue::Font;
use vte::{Params, Perform};
use winit::keyboard::{Key, ModifiersState, NamedKey};

#[derive(Default)]
pub(crate) struct Terminal {
    pub cursor_position: (usize, usize),
    pub cursor_size: (f32, f32),
    pub size: (usize, usize),
    pub font_size: f32,
    pub last_texture_size: wgpu::Extent3d,
    pub font: Option<Font>,
    pub texture: Option<wgpu::Texture>,
    screen: Screen,
    parser: vte::Parser,
}

impl Terminal {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();

        let font = database
            .faces()
            .filter(|face| face.monospaced)
            .find_map(|face| {
                database
                    .with_face_data(face.id, |data, face_index| {
                        Font::from_bytes(
                            data,
                            fontdue::FontSettings {
                                collection_index: face_index,
                                ..Default::default()
                            },
                        )
                        .ok()
                    })
                    .flatten()
            })
            .ok_or_else(|| anyhow::anyhow!("no usable monospace font found"))?;
        let font_size = 16.0;
        let cell_width = font.metrics('a', font_size).advance_width;
        let cell_height = font
            .horizontal_line_metrics(font_size)
            .expect("horizontal font")
            .new_line_size;

        let mut terminal = Self {
            font: Some(font),
            font_size,
            cursor_size: (cell_width, cell_height),
            ..Default::default()
        };

        // Anything the shell writes before the first frame is parsed into this
        // starting grid; `update_layout` resizes it to the window afterwards
        // without losing the text.
        terminal.screen.resize(INITIAL_COLS as usize, INITIAL_ROWS as usize);

        Ok(terminal)
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
        self.update_cursor_position();
    }

    fn update_cursor_position(&mut self) {
        self.cursor_position = self.screen.cursor();
    }

    fn update_grid_size(&mut self, width: usize, height: usize) {
        let Some(font) = self.font.as_ref() else {
            return;
        };
        let cell_width = font.metrics('a', self.font_size).advance_width.max(1.0);
        let Some(line) = font.horizontal_line_metrics(self.font_size) else {
            return;
        };
        let cell_height = line.new_line_size.max(1.0);

        self.cursor_size = (cell_width, cell_height);
        self.size = (
            (width as f32 / cell_width).floor().max(1.0) as usize,
            (height as f32 / cell_height).floor().max(1.0) as usize,
        );
        self.screen.resize(self.size.0, self.size.1);
        self.update_cursor_position();
    }

    fn rasterize(&self, w: usize, h: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; w * h * 4];

        let Some(font) = self.font.as_ref() else {
            return buffer;
        };

        let (cols, rows) = self.size;
        let (cell_width, cell_height) = self.cursor_size;
        if cols == 0 || rows == 0 || cell_width <= 0.0 || cell_height <= 0.0 {
            return buffer;
        }
        let Some(line) = font.horizontal_line_metrics(self.font_size) else {
            return buffer;
        };

        for row in 0..rows {
            for col in 0..cols {
                let ch = self.screen.cell(row, col);

                // Blank cells are the common case: skip rasterizing them.
                if ch == ' ' || ch == '\0' {
                    continue;
                }

                let (metrics, bitmap) = font.rasterize(ch, self.font_size);

                if metrics.width == 0 || metrics.height == 0 {
                    continue;
                }

                let baseline = row as f32 * cell_height + line.ascent;
                let left = (col as f32 * cell_width + metrics.xmin as f32).round() as i32;
                let top = (baseline - metrics.ymin as f32 - metrics.height as f32).round() as i32;

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

                        let i = (y as usize * w + x as usize) * 4;

                        buffer[i] = coverage;
                        buffer[i + 1] = coverage;
                        buffer[i + 2] = coverage;
                        buffer[i + 3] = 255;
                    }
                }
            }
        }

        buffer
    }
}

/// The character grid the VT parser draws into, plus the little state it needs
/// (the cursor and a pending wrap).
///
/// Cells are plain `char`s on purpose: this is the smallest model that renders
/// correct text. Attributes (colour, bold), wide glyphs, scrollback and the
/// alternate screen are the next things to add.
#[derive(Default)]
struct Screen {
    cells: Vec<char>,
    cols: usize,
    rows: usize,
    /// (column, row)
    cursor: (usize, usize),
    /// Set once the cursor reaches the right margin. The wrap happens when the
    /// *next* character arrives, which is what real terminals do and what keeps
    /// a full line from scrolling a row too early.
    wrap_pending: bool,
}

impl Screen {
    const BLANK: char = ' ';

    fn cursor(&self) -> (usize, usize) {
        self.cursor
    }

    fn cell(&self, row: usize, col: usize) -> char {
        self.cells
            .get(row * self.cols + col)
            .copied()
            .unwrap_or(Self::BLANK)
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }

        let mut cells = vec![Self::BLANK; cols * rows];

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
        self.cells[row * self.cols + col] = ch;

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

        if self.rows == 1 {
            self.cells.fill(Self::BLANK);
            return;
        }

        self.cells.drain(..self.cols);
        self.cells.resize(self.cols * self.rows, Self::BLANK);
    }

    fn scroll_down(&mut self) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        if self.rows == 1 {
            self.cells.fill(Self::BLANK);
            return;
        }

        self.cells.truncate(self.cols * (self.rows - 1));
        self.cells.resize(self.cols * self.rows, Self::BLANK);
        self.cells.rotate_right(self.cols);
    }

    fn erase_display(&mut self, mode: u16) {
        if self.cells.is_empty() {
            return;
        }

        let index = self.cursor.1 * self.cols + self.cursor.0;

        match mode {
            0 => self.cells[index..].fill(Self::BLANK),
            1 => self.cells[..=index].fill(Self::BLANK),
            _ => self.cells.fill(Self::BLANK),
        }
    }

    fn erase_line(&mut self, mode: u16) {
        if self.cols == 0 || self.rows == 0 {
            return;
        }

        let start = self.cursor.1 * self.cols;

        match mode {
            0 => self.cells[start + self.cursor.0..start + self.cols].fill(Self::BLANK),
            1 => self.cells[start..=start + self.cursor.0].fill(Self::BLANK),
            _ => self.cells[start..start + self.cols].fill(Self::BLANK),
        }
    }

    #[cfg(test)]
    fn line(&self, row: usize) -> String {
        (0..self.cols).map(|col| self.cell(row, col)).collect()
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
            0x0a | 0x0b | 0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            // BEL, SO, SI and friends change nothing on screen yet.
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
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
            // SGR (colour), mode set/reset and device status are ignored for
            // now; they are consumed here so they never reach the grid as text.
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
    use super::{Screen, Terminal, encode_key};
    use vte::Parser;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

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
        let mut terminal = Terminal::new().expect("a system monospace font");
        terminal.update_grid_size(256, 64);
        terminal.feed(b"A");

        let pixels = terminal.rasterize(256, 64);

        assert!(pixels.chunks_exact(4).any(|pixel| pixel[0] != 0));
    }

    #[test]
    fn resizing_rebuilds_the_grid_without_losing_the_text() {
        let mut terminal = Terminal::new().expect("a system monospace font");
        terminal.update_grid_size(1024, 640);
        terminal.feed(b"ab\tc\r\nX");

        let large_grid = terminal.size;
        terminal.update_grid_size(512, 320);

        assert!(terminal.size.0 < large_grid.0);
        assert!(terminal.size.1 < large_grid.1);
        assert_eq!(terminal.screen.line(0).trim_end(), "ab      c");
        assert_eq!(terminal.screen.line(1).trim_end(), "X");
    }
}

