use crate::{WINDOW_TITLE, gpu::Gpu};
use fontdue::Font;

#[derive(Default)]
pub(crate) struct Terminal {
    pub buffer: Vec<u8>,
    pub cursor_position: (usize, usize),
    pub cursor_size: (f32, f32),
    pub cursor_index: usize,
    pub color: (f32, f32, f32),
    pub size: (usize, usize),
    pub font_size: f32,
    pub last_texture_size: wgpu::Extent3d,
    pub font: Option<Font>,
    pub texture: Option<wgpu::Texture>,
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

        Ok(Self {
            font: Some(font),
            font_size,
            cursor_size: (cell_width, cell_height),
            ..Default::default()
        })
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

    pub fn handle_key_event(&mut self, key: winit::keyboard::Key) {
        use winit::keyboard::{Key, NamedKey};

        match key {
            Key::Named(NamedKey::Backspace) => {
                if self.cursor_index > 0 {
                    let previous = self.previous_char_boundary(self.cursor_index);
                    self.buffer.drain(previous..self.cursor_index);
                    self.cursor_index = previous;
                }
            }
            Key::Named(NamedKey::Delete) => {
                if self.cursor_index < self.buffer.len() {
                    let next = self.next_char_boundary(self.cursor_index);
                    self.buffer.drain(self.cursor_index..next);
                }
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.cursor_index = self.previous_char_boundary(self.cursor_index);
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.cursor_index = self.next_char_boundary(self.cursor_index);
            }
            Key::Named(NamedKey::ArrowUp) => self.move_cursor_vertical(false),
            Key::Named(NamedKey::ArrowDown) => self.move_cursor_vertical(true),
            Key::Named(NamedKey::Home) => {
                self.cursor_index = self.line_start(self.cursor_index);
            }
            Key::Named(NamedKey::End) => {
                self.cursor_index = self.line_end(self.cursor_index);
            }
            Key::Named(NamedKey::Enter) => self.insert_text("\n"),
            Key::Named(NamedKey::Tab) => self.insert_text("\t"),
            Key::Character(k) => {
                self.insert_text(k.as_str());
            }
            _ => {}
        }

        self.update_cursor_position();
    }

    fn insert_text(&mut self, text: &str) {
        let bytes = text.as_bytes();
        self.buffer
            .splice(self.cursor_index..self.cursor_index, bytes.iter().copied());
        self.cursor_index += bytes.len();
    }

    fn previous_char_boundary(&self, index: usize) -> usize {
        let mut previous = index.saturating_sub(1);
        while previous > 0 && self.buffer[previous] & 0b1100_0000 == 0b1000_0000 {
            previous -= 1;
        }
        previous
    }

    fn next_char_boundary(&self, index: usize) -> usize {
        let mut next = (index + 1).min(self.buffer.len());
        while next < self.buffer.len() && self.buffer[next] & 0b1100_0000 == 0b1000_0000 {
            next += 1;
        }
        next
    }

    fn line_start(&self, index: usize) -> usize {
        self.buffer[..index]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |newline| newline + 1)
    }

    fn line_end(&self, index: usize) -> usize {
        self.buffer[index..]
            .iter()
            .position(|&byte| byte == b'\n')
            .map_or(self.buffer.len(), |offset| index + offset)
    }

    fn move_cursor_vertical(&mut self, down: bool) {
        let current_start = self.line_start(self.cursor_index);
        let column = self.cursor_index - current_start;

        let target_start = if down {
            let current_end = self.line_end(self.cursor_index);
            if current_end == self.buffer.len() {
                return;
            }
            current_end + 1
        } else {
            if current_start == 0 {
                return;
            }
            let previous_end = current_start - 1;
            self.line_start(previous_end)
        };
        let target_end = self.line_end(target_start);
        self.cursor_index = (target_start + column).min(target_end);
    }

    fn update_cursor_position(&mut self) {
        let cols = self.size.0;
        if cols == 0 {
            self.cursor_position = (0, 0);
            return;
        }

        let cursor_index = self.cursor_index.min(self.buffer.len());
        let cell_index = String::from_utf8_lossy(&self.buffer[..cursor_index])
            .chars()
            .fold(0, |index, ch| Self::advance_cell_index(index, ch, cols));
        self.cursor_position = (cell_index % cols, cell_index / cols);
    }

    fn advance_cell_index(index: usize, ch: char, cols: usize) -> usize {
        match ch {
            '\n' => (index / cols + 1) * cols,
            '\r' => (index / cols) * cols,
            '\t' => index / cols * cols + (index % cols + 4) / 4 * 4,
            _ if ch.is_control() => index,
            _ => index + 1,
        }
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

        let mut cell_index = 0;
        for ch in String::from_utf8_lossy(&self.buffer).chars() {
            let row = cell_index / cols;
            let col = cell_index % cols;
            if row >= rows {
                break;
            }

            if ch.is_control() {
                cell_index = Self::advance_cell_index(cell_index, ch, cols);
                continue;
            }
            cell_index = Self::advance_cell_index(cell_index, ch, cols);

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

        buffer
    }
}

#[cfg(test)]
mod tests {
    use super::Terminal;
    use winit::keyboard::{Key, NamedKey};

    #[test]
    fn special_keys_edit_text_and_move_the_cursor() {
        let mut terminal = Terminal::default();
        terminal.handle_key_event(Key::Character("abc".into()));
        terminal.handle_key_event(Key::Named(NamedKey::ArrowLeft));
        terminal.handle_key_event(Key::Named(NamedKey::Backspace));
        terminal.handle_key_event(Key::Named(NamedKey::Delete));

        assert_eq!(terminal.buffer, b"a");

        terminal.handle_key_event(Key::Named(NamedKey::Home));
        terminal.handle_key_event(Key::Named(NamedKey::Enter));
        terminal.handle_key_event(Key::Named(NamedKey::Tab));
        terminal.handle_key_event(Key::Character("x".into()));

        assert_eq!(terminal.buffer, b"\n\txa");

        terminal.handle_key_event(Key::Named(NamedKey::End));
        assert_eq!(terminal.cursor_index, 4);
        terminal.handle_key_event(Key::Named(NamedKey::ArrowUp));
        assert_eq!(terminal.cursor_index, 0);
        terminal.handle_key_event(Key::Named(NamedKey::ArrowDown));
        assert_eq!(terminal.cursor_index, 1);
        terminal.handle_key_event(Key::Named(NamedKey::ArrowRight));
        terminal.handle_key_event(Key::Character("é".into()));
        terminal.handle_key_event(Key::Named(NamedKey::Backspace));

        assert_eq!(terminal.buffer, b"\n\txa");
    }

    #[test]
    fn configured_terminal_rasterizes_typed_characters() {
        let mut terminal = Terminal::new().expect("a system monospace font");
        terminal.update_grid_size(256, 64);
        terminal.handle_key_event(Key::Character("A".into()));

        let pixels = terminal.rasterize(256, 64);

        assert!(pixels.chunks_exact(4).any(|pixel| pixel[0] != 0));
    }

    #[test]
    fn cursor_matches_fitted_text_cell_after_tabs_and_newlines() {
        let mut terminal = Terminal::new().expect("a system monospace font");
        terminal.update_grid_size(1024, 640);
        terminal.handle_key_event(Key::Character("ab\tc\nX".into()));
        assert_eq!(terminal.cursor_position, (1, 1));

        let large_grid = terminal.size;
        terminal.update_grid_size(512, 320);
        assert!(terminal.size.0 < large_grid.0);
        assert!(terminal.size.1 < large_grid.1);
    }
}
