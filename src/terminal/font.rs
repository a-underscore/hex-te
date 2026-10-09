//! The faces the grid is drawn with, kept apart from the grid so they can be
//! an ECS component alongside the GPU and the shell.

use fontdue::{Font as Face, FontSettings, LineMetrics, Metrics};

/// The faces a monospaced family offers, plus the cell metrics derived from
/// the regular one.
///
/// [`Terminal`](crate::terminal::Terminal) holds a handle to this rather than a
/// copy, so the rasterization settings have exactly one owner. A family that
/// has no bold or italic face simply leaves those as `None`, and a cell that
/// asked for one is drawn with the regular face instead.
pub(crate) struct Font {
    regular: Face,
    bold: Option<Face>,
    italic: Option<Face>,
    bold_italic: Option<Face>,
    size: f32,
    cell: (f32, f32),
}

impl Font {
    /// Picks the first usable monospaced face out of the system font database,
    /// measures it at `size` logical pixels, and looks for the bold and italic
    /// faces of the same family.
    pub fn load(size: f32) -> anyhow::Result<Self> {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();

        let (info, regular) = database
            .faces()
            .filter(|face| face.monospaced)
            .find_map(|info| face(&database, info.id).map(|face| (info.clone(), face)))
            .ok_or_else(|| anyhow::anyhow!("no usable monospace font found"))?;

        let family = info.families.first().map(|(name, _)| name.clone());

        // The bold and italic faces are only worth having when they are the
        // same family as the regular one: another family's advance width would
        // move the text around inside the cell.
        let sibling = |weight: fontdb::Weight, style: fontdb::Style| -> Option<Face> {
            let family = family.as_deref()?;

            database
                .faces()
                .find(|info| {
                    info.monospaced
                        && info.weight == weight
                        && (info.style == style
                            || (style == fontdb::Style::Italic
                                && info.style == fontdb::Style::Oblique))
                        && info
                            .families
                            .iter()
                            .any(|(name, _)| name.as_str() == family)
                })
                .and_then(|info| face(&database, info.id))
        };

        Ok(Self::with_faces(
            regular,
            sibling(fontdb::Weight::BOLD, fontdb::Style::Normal),
            sibling(fontdb::Weight::NORMAL, fontdb::Style::Italic),
            sibling(fontdb::Weight::BOLD, fontdb::Style::Italic),
            size,
        ))
    }

    fn with_faces(
        regular: Face,
        bold: Option<Face>,
        italic: Option<Face>,
        bold_italic: Option<Face>,
        size: f32,
    ) -> Self {
        let cell = (
            regular.metrics('a', size).advance_width.max(1.0),
            regular
                .horizontal_line_metrics(size)
                .map(|line| line.new_line_size)
                .unwrap_or(size)
                .max(1.0),
        );

        Self {
            regular,
            bold,
            italic,
            bold_italic,
            size,
            cell,
        }
    }

    /// The width and height of one character cell.
    pub fn cell(&self) -> (f32, f32) {
        self.cell
    }

    pub fn line_metrics(&self) -> Option<LineMetrics> {
        self.regular.horizontal_line_metrics(self.size)
    }

    /// The bitmap of `ch` in the face the cell's attributes ask for, falling
    /// back to the regular face when the family has no such face.
    pub fn rasterize(&self, ch: char, bold: bool, italic: bool) -> (Metrics, Vec<u8>) {
        let face = match (bold, italic) {
            (true, true) => self.bold_italic.as_ref(),
            (true, false) => self.bold.as_ref(),
            (false, true) => self.italic.as_ref(),
            (false, false) => None,
        };

        face.unwrap_or(&self.regular).rasterize(ch, self.size)
    }
}

/// Loads the face with `id` out of the database, or `None` when the file
/// cannot be read or fontdue cannot parse it.
fn face(database: &fontdb::Database, id: fontdb::ID) -> Option<Face> {
    database
        .with_face_data(id, |data, face_index| {
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
}

#[cfg(test)]
mod tests {
    use super::Font;

    #[test]
    fn loading_a_system_font_gives_a_usable_cell() {
        let font = Font::load(16.0).expect("a system monospace font");
        let (width, height) = font.cell();

        assert!(width > 0.0 && height > 0.0);
        assert!(font.line_metrics().is_some());
    }

    #[test]
    fn a_bold_or_italic_cell_still_draws_a_glyph() {
        let font = Font::load(16.0).expect("a system monospace font");

        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let (metrics, bitmap) = font.rasterize('M', bold, italic);

            assert!(metrics.width > 0 && metrics.height > 0, "bold: {bold}");
            assert_eq!(bitmap.len(), metrics.width * metrics.height);
        }
    }
}
