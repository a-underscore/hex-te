//! The screen draw: the pipeline, the bind group and the uniform a frame ends
//! with, plus the render systems a config file can add to it.
//!
//! [`Gpu`](crate::render::Gpu) owns one of these and hands it a frame it has just
//! set up; the drawable finishes it. Everything the screen is drawn with lives
//! here, which leaves the render command itself with the frame's own business:
//! asking the surface for an image, laying the grid out, and presenting.

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::anyhow;
use winit::event::Event;

use crate::WINDOW_TITLE;
use crate::terminal::{Terminal, srgb};
use crate::world::{Control, RENDER_PIPELINE, SystemManager, World};

const SCREEN_SHADER: &str = include_str!("shaders/screen.wgsl");

/// The WGSL the screen pipeline is built from: the source the config wrote, or
/// the shader compiled into the binary when the config set none.
///
/// A source that is not WGSL the compiler accepts is reported and the built-in
/// shader used instead, so a broken shader in the config file is a note rather
/// than a terminal that will not start — the same bargain every other setting
/// makes. It is checked here rather than left to wgpu because a validation
/// failure there is fatal to the process.
fn screen_shader(source: Option<&str>) -> Cow<'static, str> {
    let Some(source) = source else {
        return Cow::Borrowed(SCREEN_SHADER);
    };

    if let Err(error) = validate_shader(source) {
        eprintln!("{WINDOW_TITLE}: the shader in the config is not usable: {error:#}");

        return Cow::Borrowed(SCREEN_SHADER);
    }

    Cow::Owned(source.to_owned())
}

/// Runs the shader through the same WGSL front end wgpu uses, so that a source
/// which cannot be built is found before the pipeline is asked for it.
fn validate_shader(source: &str) -> anyhow::Result<()> {
    let module = naga::front::wgsl::parse_str(source).map_err(|error| {
        anyhow!(
            "not valid WGSL: {}",
            error.emit_to_string(source).trim_end()
        )
    })?;

    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .map_err(|error| anyhow!("not a usable shader: {}", error.emit_to_string(source)))?;

    Ok(())
}

// The shaped cursor the pipeline starts with, before a shell asks for another
// one with `DECSCUSR`; the numbers are what `src/render/shaders/screen.wgsl`
// matches on.
const CURSOR_BLOCK: u32 = 0;

/// What the overlay is when the config named no picture: nothing at all, so the
/// composite in the shader comes out as the image underneath.
const NO_OVERLAY: [u8; 4] = [0, 0, 0, 0];

/// The two pictures a config file can name: one behind the grid, one over
/// everything. Either can be absent, and a frame composited with both falls out
/// the same way in every case, because a missing picture leaves a single pixel
/// of the right kind in its place.
#[derive(Clone, Copy, Default)]
pub(crate) struct Pictures<'a> {
    pub(crate) background: Option<&'a Path>,
    pub(crate) foreground: Option<&'a Path>,
}

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
    /// Seconds since the app started: the clock an animated shader reads to
    /// move. A shader that ignores it draws the same frame every time, which is
    /// what every shader did before it existed.
    time: f32,
    /// Keeps the struct a multiple of sixteen bytes, which is the alignment a
    /// uniform buffer binding wants. The shader does not read it.
    pad: u32,
}

impl ScreenUniforms {
    /// The colours are not here: they belong to the terminal and are copied in
    /// by [`follow`](Self::follow), so that a render function can animate them.
    fn new(resolution: [f32; 2]) -> Self {
        Self {
            background: [0.0; 4],
            cursor_color: [0.0; 4],
            resolution,
            grid: [1, 1],
            cursor: [0, 0],
            cursor_visible: 1,
            cursor_style: CURSOR_BLOCK,
            cursor_size: [0.0, 0.0],
            time: 0.0,
            pad: 0,
        }
    }

    /// Points the uniform at the terminal's current state: the colours the
    /// frame composites with, how big the grid is, where the cursor sits in it,
    /// what shape it has and whether it is drawn at all.
    fn follow(&mut self, terminal: &Terminal) {
        self.background = terminal.background;
        self.cursor_color = terminal.cursor_color;
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
    /// The instant `screen.time` is counted from. The app makes one when it
    /// starts and keeps it across a config reload, so the clock a shader reads
    /// does not restart when the file is saved.
    start: Instant,
    /// The picture the grid is drawn over. It is held here because the bind
    /// group only has a view of it, and the bind group is rebuilt whenever the
    /// terminal's own texture is replaced.
    background_view: wgpu::TextureView,
    /// That same texture, held as well as its view: when the config named no
    /// picture it is one pixel of the background colour, and a render function
    /// that animates the colour rewrites that pixel.
    background_texture: wgpu::Texture,
    /// Whether `background_texture` is the config's picture rather than the
    /// one-pixel backdrop. A picture is never rewritten.
    background_is_picture: bool,
    /// The pixel `background_texture` holds now, so a frame that did not change
    /// the colour does not upload it again.
    backdrop: [u8; 4],
    /// The picture drawn over everything, held for the same reason.
    foreground_view: wgpu::TextureView,
    screen: ScreenUniforms,
}

impl Drawable {
    /// Builds the screen pipeline that every frame is drawn with.
    ///
    /// `shader` is the WGSL source the config wrote, when it wrote one —
    /// checked here, and replaced by the built-in one with a note if it is not
    /// WGSL the compiler accepts — and `pictures` are the ones it named, when
    /// it named any. `start` is the clock the shader's `time` counts from: the
    /// app's own, so that a reload does not restart it.
    pub(crate) fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        shader: Option<&str>,
        pictures: Pictures<'_>,
        start: Instant,
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
                // The picture over everything, with a transparent pixel in its
                // place when the config named none.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
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

        // The resolution is set once the frame knows how big the surface is,
        // and the colours arrive from the terminal on the first `sync`.
        let screen = ScreenUniforms::new([0.0, 0.0]);

        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(WINDOW_TITLE),
            size: std::mem::size_of::<ScreenUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::bytes_of(&screen));

        let (background_texture, background_is_picture) = picture_texture(
            device,
            queue,
            // Only ever seen for the one frame before the first `sync` puts
            // the terminal's own background colour in its place.
            [0, 0, 0, 255],
            pictures.background,
        );
        let background_view =
            background_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let foreground_view = picture_texture(device, queue, NO_OVERLAY, pictures.foreground)
            .0
            .create_view(&wgpu::TextureViewDescriptor::default());

        let bind_group = create_screen_bind_group(
            device,
            &layout,
            &buffer,
            &screen_view,
            &sampler,
            &background_view,
            &foreground_view,
        );

        Self {
            pipeline,
            layout,
            sampler,
            buffer,
            bind_group,
            start,
            background_view,
            background_texture,
            background_is_picture,
            backdrop: [0, 0, 0, 255],
            foreground_view,
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
            &self.foreground_view,
        );
    }

    /// How big the surface is, in physical pixels, which the shader needs to
    /// turn a cell into a rectangle.
    pub(crate) fn set_resolution(&mut self, resolution: [f32; 2]) {
        self.screen.resolution = resolution;
    }

    /// Writes the uniform the shader reads, from the state the terminal is in.
    ///
    /// The clock is stamped here, on the frame's own thread, so the shader's
    /// `time` is the moment this frame is drawn rather than the moment the
    /// uniform was last built.
    pub(crate) fn sync(&mut self, queue: &wgpu::Queue, terminal: &Terminal) {
        self.screen.follow(terminal);
        self.screen.time = self.start.elapsed().as_secs_f32();
        self.follow_background(queue);
        queue.write_buffer(&self.buffer, 0, bytemuck::bytes_of(&self.screen));
    }

    /// Keeps the one-pixel backdrop in step with the background colour.
    ///
    /// The colour is the terminal's, and a render function can animate it, so
    /// the pixel the shader mixes the grid over follows it. A picture the
    /// config named is left alone.
    fn follow_background(&mut self, queue: &wgpu::Queue) {
        if self.background_is_picture {
            return;
        }

        let pixel = background_pixel(self.screen.background);

        if pixel == self.backdrop {
            return;
        }

        self.backdrop = pixel;
        write_pixel(queue, &self.background_texture, pixel);
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
    foreground: &wgpu::TextureView,
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
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(foreground),
            },
        ],
    })
}

/// One of the two pictures a frame is composited with: the file the config
/// named, or `fallback` — a single pixel — so that the shader always has a
/// texture to sample. The flag says which of the two the texture came out as,
/// which is what tells a backdrop that can follow the background colour from
/// one that is a picture and must be left alone.
///
/// A picture that cannot be read is reported and the fallback used instead, the
/// way a config that cannot be evaluated keeps the defaults.
fn picture_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    fallback: [u8; 4],
    image: Option<&Path>,
) -> (wgpu::Texture, bool) {
    let (width, height, pixels, is_picture) = match image.map(load_picture) {
        Some(Ok(picture)) => {
            let (width, height, pixels) = picture;

            (width, height, pixels, true)
        }
        Some(Err(error)) => {
            eprintln!("{WINDOW_TITLE}: {error:#}");

            (1, 1, fallback.to_vec(), false)
        }
        None => (1, 1, fallback.to_vec(), false),
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

    (texture, is_picture)
}

/// Rewrites the single pixel of a one-pixel texture.
///
/// The backdrop is such a texture whenever the config named no picture, so
/// animating the background colour is a four-byte upload on the frames where
/// the colour really changed — no new texture, and no bind group to rebuild.
fn write_pixel(queue: &wgpu::Queue, texture: &wgpu::Texture, pixel: [u8; 4]) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixel,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// One pixel of the background colour, opaque: what the backdrop is when the
/// config named no picture, so the shader always has a texture to sample.
fn background_pixel(background: [f32; 4]) -> [u8; 4] {
    let rgb = srgb(background);

    [rgb[0], rgb[1], rgb[2], 255]
}

/// Reads a picture off the disk as `(width, height, RGBA8)`.
fn load_picture(path: &Path) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let picture = image::open(path)
        .map_err(|error| anyhow!("reading {}: {error}", path.display()))?
        .to_rgba8();

    Ok((picture.width(), picture.height(), picture.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::{
        CURSOR_BLOCK, NO_OVERLAY, SCREEN_SHADER, ScreenUniforms, background_pixel, load_picture,
        screen_shader, validate_shader,
    };
    use crate::terminal::Terminal;
    use crate::terminal::font::Font;

    use std::sync::{Arc, RwLock};

    #[test]
    fn the_compiled_shader_is_what_an_unset_setting_gets() {
        assert_eq!(&*screen_shader(None), SCREEN_SHADER);
    }

    #[test]
    fn the_built_in_shader_is_wgsl_the_compiler_accepts() {
        validate_shader(SCREEN_SHADER).expect("the shader the binary ships with");
    }

    #[test]
    fn the_configs_own_shader_is_used_instead() {
        let source = "@vertex fn vs_screen() -> @builtin(position) vec4<f32> {\n    return vec4<f32>(0.0);\n}\n";

        assert_eq!(&*screen_shader(Some(source)), source);
    }

    #[test]
    fn a_shader_that_is_not_wgsl_keeps_the_built_in_one() {
        // A broken shader is a note and the built-in shader, not a terminal
        // that refuses to start.
        assert_eq!(&*screen_shader(Some("this is not WGSL")), SCREEN_SHADER);
    }

    #[test]
    fn the_uniform_is_a_whole_number_of_sixteen_byte_rows() {
        // The layout `src/render/shaders/screen.wgsl` declares: two vec4s, four
        // vec2s and four scalars, padded out to the alignment a uniform buffer
        // binding wants.
        assert_eq!(std::mem::size_of::<ScreenUniforms>(), 80);
        assert_eq!(std::mem::size_of::<ScreenUniforms>() % 16, 0);
    }

    #[test]
    fn the_uniform_follows_the_terminal() {
        let font = Font::load(16.0).expect("a system monospace font");
        let mut terminal = Terminal::new(
            Arc::new(RwLock::new(font)),
            [0.05, 0.06, 0.08, 1.0],
            [0.16, 0.72, 0.72, 1.0],
        );
        let mut screen = ScreenUniforms::new([64.0, 32.0]);

        screen.follow(&terminal);

        assert_eq!(screen.cursor, [0, 0]);
        assert_eq!(screen.cursor_style, CURSOR_BLOCK);
        assert_eq!(screen.cursor_visible, 1);
        assert_eq!(screen.background, [0.05, 0.06, 0.08, 1.0]);
        assert_eq!(screen.cursor_color, [0.16, 0.72, 0.72, 1.0]);
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

        // A render function that recolours the terminal shows up here, which is
        // the frame after it ran.
        terminal.set_background([1.0, 0.0, 0.0, 1.0]);
        terminal.set_cursor_color([0.0, 1.0, 0.0, 1.0]);
        screen.follow(&terminal);

        assert_eq!(screen.background, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(screen.cursor_color, [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn a_config_without_a_picture_gets_one_pixel_of_the_background() {
        let pixel = background_pixel([1.0, 0.0, 0.0, 1.0]);

        assert_eq!(pixel, [255, 0, 0, 255], "one pixel, and opaque");
    }

    #[test]
    fn a_config_without_an_overlay_gets_a_see_through_pixel() {
        assert_eq!(NO_OVERLAY, [0, 0, 0, 0], "nothing over the image");
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

        let (width, height, read) = load_picture(&path).expect("reading it back");

        std::fs::remove_file(&path).ok();

        assert_eq!((width, height), (2, 2));
        assert_eq!(read, pixels, "in reading order, row by row");
    }

    #[test]
    fn a_picture_that_is_not_there_names_itself_in_the_error() {
        let path = std::env::temp_dir().join("hext-no-such-picture.png");
        let error = load_picture(&path).expect_err("a picture that is not there");

        assert!(error.to_string().contains("hext-no-such-picture.png"));
    }
}
