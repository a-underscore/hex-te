//! The screen draw: the pipeline, the bind group and the uniform a frame ends
//! with, plus the render systems a config file can add to it.
//!
//! [`Gpu`](crate::gpu::Gpu) owns one of these and hands it a frame it has just
//! set up; the drawable finishes it. Everything the screen is drawn with lives
//! here, which leaves the render command itself with the frame's own business:
//! asking the surface for an image, laying the grid out, and presenting.

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, RwLock};

use anyhow::anyhow;
use winit::event::Event;

use crate::WINDOW_TITLE;
use crate::control::Control;
use crate::terminal::{Terminal, srgb};
use crate::world::{RENDER_PIPELINE, SystemManager, World};

const SCREEN_SHADER: &str = include_str!("shaders/screen.wgsl");

/// The WGSL the screen pipeline is built from: the source the config wrote, or
/// the shader compiled into the binary when the config set none.
///
/// The config is read at startup, so changing the shader is a restart rather than
/// a rebuild.
fn screen_shader(source: Option<&str>) -> Cow<'static, str> {
    match source {
        Some(source) => Cow::Owned(source.to_owned()),
        None => Cow::Borrowed(SCREEN_SHADER),
    }
}

// The shaped cursor the pipeline starts with, before a shell asks for another
// one with `DECSCUSR`; the numbers are what `src/shaders/screen.wgsl` matches on.
const CURSOR_BLOCK: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScreenUniforms {
    background: [f32; 4],
    cursor_color: [f32; 4],
    resolution: [f32; 2],
    grid: [u32; 2],
    cursor: [u32; 2],
    cursor_visible: u32,
    cursor_style: u32,
    cursor_size: [f32; 2],
    padding: [u32; 2],
}

impl ScreenUniforms {
    fn new(background: [f32; 4], cursor_color: [f32; 4], resolution: [f32; 2]) -> Self {
        Self {
            background,
            cursor_color,
            resolution,
            grid: [1, 1],
            cursor: [0, 0],
            cursor_visible: 1,
            cursor_style: CURSOR_BLOCK,
            cursor_size: [0.0, 0.0],
            padding: [0; 2],
        }
    }

    /// Points the uniform at the terminal's current state: how big the grid is,
    /// where the cursor sits in it, what shape it has and whether it is drawn
    /// at all.
    fn follow(&mut self, terminal: &Terminal) {
        self.cursor = [
            terminal.cursor_position.0 as u32,
            terminal.cursor_position.1 as u32,
        ];
        self.grid = [terminal.size.0 as u32, terminal.size.1 as u32];
        self.cursor_size = [terminal.cursor_size.0, terminal.cursor_size.1];
        self.cursor_visible = u32::from(terminal.cursor_visible);
        self.cursor_style = terminal.cursor_style;
    }
}

/// The draw a frame ends with.
///
/// It owns the screen pipeline, the bind group that holds the grid's texture
/// and the uniform the shader reads, so that a frame can be finished by calling
/// [`Drawable::draw`] and nothing else.
pub(crate) struct Drawable {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// The picture the grid is drawn over. It is held here because the bind
    /// group only has a view of it, and the bind group is rebuilt whenever the
    /// terminal's own texture is replaced.
    background_view: wgpu::TextureView,
    screen: ScreenUniforms,
}

impl Drawable {
    /// Builds the screen pipeline that every frame is drawn with.
    ///
    /// `background` and `cursor_color` come from the config file, as `r, g, b, a`,
    /// `shader` is the WGSL source the config wrote, when it wrote one, and
    /// `background_image` is the picture it wants behind the grid, when it named
    /// one.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        background: [f32; 4],
        cursor_color: [f32; 4],
        shader: Option<&str>,
        background_image: Option<&Path>,
    ) -> Self {
        let screen_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(WINDOW_TITLE),
            source: wgpu::ShaderSource::Wgsl(screen_shader(shader)),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(WINDOW_TITLE),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // The picture behind the grid. A config that named none still
                // gets a texture here: one pixel of the background colour, so
                // the shader has nothing to special-case.
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(WINDOW_TITLE),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(WINDOW_TITLE),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &screen_module,
                entry_point: Some("vs_screen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &screen_module,
                entry_point: Some("fs_screen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

        // The screen starts out as a one-pixel texture: the first frame the
        // terminal lays out replaces it.
        let placeholder = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(WINDOW_TITLE),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let screen_view = placeholder.create_view(&wgpu::TextureViewDescriptor::default());

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(WINDOW_TITLE),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let screen = ScreenUniforms::new(
            background,
            cursor_color,
            [0.0, 0.0], // resolved once the frame knows how big the surface is
        );

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(WINDOW_TITLE),
            size: std::mem::size_of::<ScreenUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&screen));

        let background_view = background_texture(device, queue, background, background_image)
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = create_screen_bind_group(
            device,
            &layout,
            &buffer,
            &screen_view,
            &sampler,
            &background_view,
        );

        Self {
            pipeline,
            layout,
            sampler,
            buffer,
            bind_group,
            background_view,
            screen,
        }
    }

    /// Points the bind group at a new screen texture.
    ///
    /// wgpu textures cannot be resized, so a terminal that grew or shrank has a
    /// new texture, and the bind group holding the old one has to be rebuilt.
    pub(crate) fn bind_texture(&mut self, device: &wgpu::Device, texture: &wgpu::Texture) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.bind_group = create_screen_bind_group(
            device,
            &self.layout,
            &self.buffer,
            &view,
            &self.sampler,
            &self.background_view,
        );
    }

    /// How big the surface is, in physical pixels, which the shader needs to
    /// turn a cell into a rectangle.
    pub(crate) fn set_resolution(&mut self, resolution: [f32; 2]) {
        self.screen.resolution = resolution;
    }

    /// Writes the uniform the shader reads, from the state the terminal is in.
    pub(crate) fn sync(&mut self, queue: &wgpu::Queue, terminal: &Terminal) {
        self.screen.follow(terminal);
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&self.screen));
    }

    /// Finishes the frame: the render systems a config file registered run
    /// first, so that whatever they did to the world is what gets drawn, and
    /// the screen goes over the cleared image after that.
    ///
    /// The world and its systems are what a render function needs to say what
    /// is drawn — the terminal itself is a component of that world, so a
    /// function written in Python reaches it the way it reaches anything else.
    pub(crate) fn draw<E: 'static>(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        world: Arc<RwLock<World<E>>>,
        sm: SystemManager<E>,
    ) -> anyhow::Result<()> {
        // A frame is not an event, so the render systems are handed the one
        // that says the loop has nothing left to do — which is exactly when a
        // frame is being drawn.
        sm.update_pipeline(RENDER_PIPELINE, Control::new(Event::AboutToWait), world)?;

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(WINDOW_TITLE),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: self.screen.background[0] as f64,
                        g: self.screen.background[1] as f64,
                        b: self.screen.background[2] as f64,
                        a: self.screen.background[3] as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);

        Ok(())
    }
}

fn create_screen_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    background: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(WINDOW_TITLE),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(background),
            },
        ],
    })
}

/// The picture the grid is drawn over: the file the config named, or a single
/// pixel of the configured background colour, so that the shader always has one
/// to sample. A picture that cannot be read is reported and the colour used
/// instead, the way a config that cannot be evaluated keeps the defaults.
fn background_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    background: [f32; 4],
    image: Option<&Path>,
) -> wgpu::Texture {
    let (width, height, pixels) = match image.map(load_background) {
        Some(Ok(picture)) => picture,
        Some(Err(error)) => {
            eprintln!("{WINDOW_TITLE}: {error:#}");

            (1, 1, background_pixel(background))
        }
        None => (1, 1, background_pixel(background)),
    };

    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(WINDOW_TITLE),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        size,
    );

    texture
}

/// One pixel of the background colour, opaque: what the backdrop is when the
/// config named no picture, so the shader always has a texture to sample.
fn background_pixel(background: [f32; 4]) -> Vec<u8> {
    let mut pixel = srgb(background).to_vec();

    pixel.push(255);

    pixel
}

/// Reads a picture off the disk as `(width, height, RGBA8)`.
fn load_background(path: &Path) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let picture = image::open(path)
        .map_err(|error| anyhow!("reading {}: {error}", path.display()))?
        .to_rgba8();

    Ok((picture.width(), picture.height(), picture.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::{
        CURSOR_BLOCK, SCREEN_SHADER, ScreenUniforms, background_pixel, load_background,
        screen_shader,
    };
    use crate::font::Font;
    use crate::terminal::Terminal;

    use std::sync::{Arc, RwLock};

    #[test]
    fn the_compiled_shader_is_what_an_unset_setting_gets() {
        assert_eq!(&*screen_shader(None), SCREEN_SHADER);
    }

    #[test]
    fn the_configs_own_shader_is_used_instead() {
        let source = "// the WGSL the config wrote\n";

        assert_eq!(&*screen_shader(Some(source)), source);
    }

    #[test]
    fn the_uniform_follows_the_terminal() {
        let font = Font::load(16.0).expect("a system monospace font");
        let mut terminal = Terminal::new(Arc::new(RwLock::new(font)), [0.05, 0.06, 0.08, 1.0]);
        let mut screen = ScreenUniforms::new([0.0; 4], [1.0; 4], [64.0, 32.0]);

        screen.follow(&terminal);

        assert_eq!(screen.cursor, [0, 0]);
        assert_eq!(screen.cursor_style, CURSOR_BLOCK);
        assert_eq!(screen.cursor_visible, 1);
        assert_eq!(
            screen.cursor_size,
            [terminal.cursor_size.0, terminal.cursor_size.1]
        );

        // Move the cursor along and hide it: the uniform says so next frame.
        terminal.feed(b"ab");
        terminal.feed(b"\x1b[?25l\x1b[5 q");
        screen.follow(&terminal);

        assert_eq!(screen.cursor, [2, 0]);
        assert_eq!(screen.cursor_visible, 0);
        assert_eq!(screen.cursor_style, 1, "the bar `DECSCUSR` asked for");
    }

    #[test]
    fn a_config_without_a_picture_gets_one_pixel_of_the_background() {
        let pixel = background_pixel([1.0, 0.0, 0.0, 1.0]);

        assert_eq!(pixel, [255, 0, 0, 255], "one pixel, and opaque");
    }

    #[test]
    fn a_picture_is_read_as_its_own_pixels() {
        use image::{ExtendedColorType, ImageEncoder, codecs::png::PngEncoder};

        let pixels = [
            255, 0, 0, 255, // top left, red
            0, 255, 0, 255, // top right, green
            0, 0, 255, 255, // bottom left, blue
            255, 255, 255, 255, // bottom right, white
        ];
        let path = std::env::temp_dir().join(format!("hext-picture-{}.png", std::process::id()));
        let file = std::fs::File::create(&path).expect("a file to write into");

        PngEncoder::new(file)
            .write_image(&pixels, 2, 2, ExtendedColorType::Rgba8)
            .expect("writing the picture");

        let (width, height, read) = load_background(&path).expect("reading it back");

        std::fs::remove_file(&path).ok();

        assert_eq!((width, height), (2, 2));
        assert_eq!(read, pixels, "in reading order, row by row");
    }

    #[test]
    fn a_picture_that_is_not_there_names_itself_in_the_error() {
        let path = std::env::temp_dir().join("hext-no-such-picture.png");
        let error = load_background(&path).expect_err("a picture that is not there");

        assert!(error.to_string().contains("hext-no-such-picture.png"));
    }
}
