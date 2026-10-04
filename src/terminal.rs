use fontdue::Font;

use crate::{WINDOW_TITLE, gpu::Gpu};

#[derive(Default)]
pub(crate) struct Terminal {
    pub buffer: Vec<u8>,
    pub cursor_position: (usize, usize),
    pub color: (f32, f32, f32),
    pub size: (usize, usize),
    pub font_size: f32,
    pub last_texture_size: wgpu::Extent3d,
    pub font: Option<Font>,
    pub texture: Option<wgpu::Texture>,
}

impl Terminal {
    pub(crate) fn update_layout(&mut self, gpu: &Gpu, width: u32, height: u32) -> bool {
        if width == 0 || height == 0 {
            return false;
        }

        let (w, h) = (width as usize, height as usize);
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

    fn rasterize(&self, w: usize, h: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; w * h * 4];

        let Some(font) = self.font.as_ref() else {
            return buffer;
        };

        let (cols, rows) = self.size;

        if cols == 0 || rows == 0 {
            return buffer;
        }

        let line = font
            .horizontal_line_metrics(self.font_size)
            .expect("horizontal font");
        let cell_w = font
            .metrics('a', self.font_size)
            .advance_width
            .ceil()
            .max(1.0);
        let cell_h = line.new_line_size.ceil().max(1.0);

        'rows: for row in 0..rows {
            for col in 0..cols {
                let Some(&byte) = self.buffer.get(row * cols + col) else {
                    break 'rows;
                };

                let ch = byte as char;

                if !(' '..='~').contains(&ch) {
                    continue;
                }

                let (metrics, bitmap) = font.rasterize(ch, self.font_size);

                if metrics.width == 0 || metrics.height == 0 {
                    continue;
                }

                let baseline = row as f32 * cell_h + line.ascent;
                let left = (col as f32 * cell_w + metrics.xmin as f32).round() as i32;
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
