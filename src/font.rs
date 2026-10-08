//! The face the grid is drawn with, kept apart from the grid so it can be an
//! ECS component alongside the GPU and the shell.

use fontdue::{Font as Face, FontSettings, LineMetrics, Metrics};

/// The size glyphs are rasterized at.
const FONT_SIZE: f32 = 16.0;

/// A monospaced face plus the cell metrics derived from it.
///
/// [`Terminal`](crate::terminal::Terminal) holds a handle to this rather than a
/// copy, so the rasterization settings have exactly one owner.
pub(crate) struct Font {
    face: Face,
    size: f32,
    cell: (f32, f32),
}

impl Font {
    /// Picks the first usable monospaced face out of the system font database.
    pub fn load() -> anyhow::Result<Self> {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();

        let face = database
            .faces()
            .filter(|face| face.monospaced)
            .find_map(|face| {
                database
                    .with_face_data(face.id, |data, face_index| {
                        Face::from_bytes(
                            data,
                            FontSettings {
                                collection_index: face_index,
                                ..Default::default()
                            },
                        )
                        .ok()
                    })
                    .flatten()
            })
            .ok_or_else(|| anyhow::anyhow!("no usable monospace font found"))?;

        Ok(Self::with_face(face))
    }

    fn with_face(face: Face) -> Self {
        let size = FONT_SIZE;
        let cell = (
            face.metrics('a', size).advance_width.max(1.0),
            face.horizontal_line_metrics(size)
                .map(|line| line.new_line_size)
                .unwrap_or(size)
                .max(1.0),
        );

        Self { face, size, cell }
    }

    /// The width and height of one character cell.
    pub fn cell(&self) -> (f32, f32) {
        self.cell
    }

    pub fn line_metrics(&self) -> Option<LineMetrics> {
        self.face.horizontal_line_metrics(self.size)
    }

    pub fn rasterize(&self, ch: char) -> (Metrics, Vec<u8>) {
        self.face.rasterize(ch, self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::Font;

    #[test]
    fn loading_a_system_font_gives_a_usable_cell() {
        let font = Font::load().expect("a system monospace font");
        let (width, height) = font.cell();

        assert!(width > 0.0 && height > 0.0);
        assert!(font.line_metrics().is_some());
    }
}
