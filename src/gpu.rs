use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use anyhow::anyhow;
use winit::window::Window;

use crate::{WINDOW_TITLE, terminal::Terminal};

const SCREEN_SHADER: &str = include_str!("shaders/screen.wgsl");

/// The WGSL the screen pipeline is built from.
///
/// A config that names a file is read here, at startup, which makes editing the
/// shader a restart instead of a rebuild. Anything wrong with the name — a file
/// that is missing or cannot be read — is reported and the shader compiled into
/// the binary is used instead, so a bad path never leaves the terminal without a
/// pipeline at all.
fn screen_shader(path: Option<&Path>) -> Cow<'static, str> {
    let Some(path) = path else {
        return Cow::Borrowed(SCREEN_SHADER);
    };

    match std::fs::read_to_string(path) {
        Ok(source) => Cow::Owned(source),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: reading {}: {error}", path.display());

            Cow::Borrowed(SCREEN_SHADER)
        }
    }
}

// The shaped cursor the pipeline starts with, before a shell asks for another
// one with `DECSCUSR`. The numbers are the ones `src/shaders/screen.wgsl`
// matches on, and `terminal::CursorStyle::shape` produces.
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

pub(crate) struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    screen_pipeline: wgpu::RenderPipeline,
    screen_buffer: wgpu::Buffer,
    screen_bind_group: wgpu::BindGroup,
    screen_layout: wgpu::BindGroupLayout,
    screen_sampler: wgpu::Sampler,
    screen: ScreenUniforms,
}

fn create_screen_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
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
        ],
    })
}

impl Gpu {
    /// Opens the surface on `window` and builds the screen pipeline.
    ///
    /// `background` and `cursor_color` come from the config file, as `r, g, b, a`,
    /// and `shader` is the WGSL file the config named, when it named one.
    pub(crate) async fn new(
        window: Arc<Window>,
        background: [f32; 4],
        cursor_color: [f32; 4],
        shader: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());

        let surface = instance.create_surface(window.clone())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                apply_limit_buckets: false,
            })
            .await?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some(WINDOW_TITLE),
                ..Default::default()
            })
            .await?;

        let size = window.inner_size();
        let config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow!("the adapter cannot drive this surface"))?;
        surface.configure(&device, &config);

        let screen_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(WINDOW_TITLE),
            source: wgpu::ShaderSource::Wgsl(screen_shader(shader)),
        });

        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(WINDOW_TITLE),
            bind_group_layouts: &[Some(&screen_layout)],
            immediate_size: 0,
        });

        let screen_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
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
                targets: &[Some(config.format.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

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

        let screen_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(WINDOW_TITLE),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let screen = ScreenUniforms {
            background,
            cursor_color,
            resolution: [config.width as f32, config.height as f32],
            grid: [1, 1],
            cursor: [0, 0],
            cursor_visible: 1,
            cursor_style: CURSOR_BLOCK,
            cursor_size: [0.0, 0.0],
            padding: [0; 2],
        };

        let screen_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(WINDOW_TITLE),
            size: std::mem::size_of::<ScreenUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&screen_buffer, 0, bytemuck::bytes_of(&screen));

        let screen_bind_group = create_screen_bind_group(
            &device,
            &screen_layout,
            &screen_buffer,
            &screen_view,
            &screen_sampler,
        );

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            screen_pipeline,
            screen_buffer,
            screen_bind_group,
            screen_layout,
            screen_sampler,
            screen,
        })
    }

    pub(crate) fn bind_terminal_texture(&mut self, texture: &wgpu::Texture) {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        self.screen_bind_group = create_screen_bind_group(
            &self.device,
            &self.screen_layout,
            &self.screen_buffer,
            &view,
            &self.screen_sampler,
        );
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    pub(crate) fn request_redraw(&self) {
        self.window.request_redraw();
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);

        self.screen.resolution = [width as f32, height as f32];
        self.queue
            .write_buffer(&self.screen_buffer, 0, bytemuck::bytes_of(&self.screen));
    }

    pub(crate) fn render(&mut self, terminal: &mut Terminal) -> anyhow::Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(WINDOW_TITLE),
            });

        {
            let dimensions = self.window.inner_size();

            if terminal.update_layout(&*self, dimensions.width, dimensions.height)
                && let Some(texture) = terminal.texture.as_ref()
            {
                self.bind_terminal_texture(texture);
            }
        }

        self.screen.cursor = [
            terminal.cursor_position.0 as u32,
            terminal.cursor_position.1 as u32,
        ];
        self.screen.grid = [terminal.size.0 as u32, terminal.size.1 as u32];
        self.screen.cursor_size = [terminal.cursor_size.0, terminal.cursor_size.1];
        self.screen.cursor_visible = u32::from(terminal.cursor_visible);
        self.screen.cursor_style = terminal.cursor_style;
        self.queue
            .write_buffer(&self.screen_buffer, 0, bytemuck::bytes_of(&self.screen));

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(WINDOW_TITLE),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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

            pass.set_pipeline(&self.screen_pipeline);
            pass.set_bind_group(0, &self.screen_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{SCREEN_SHADER, screen_shader};
    use std::path::{Path, PathBuf};

    /// A path under the temp dir that this test owns.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("hext-shader-{}-{name}.wgsl", std::process::id()))
    }

    #[test]
    fn the_compiled_shader_is_what_no_setting_gets() {
        assert_eq!(&*screen_shader(None), SCREEN_SHADER);
    }

    #[test]
    fn a_named_shader_file_is_loaded_instead() {
        let path = scratch("named");
        std::fs::write(&path, "// the file the config named\n").unwrap();

        assert_eq!(
            &*screen_shader(Some(&path)),
            "// the file the config named\n"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_shader_file_that_cannot_be_read_falls_back() {
        let missing = Path::new("/nonexistent/hext/screen.wgsl");

        assert_eq!(&*screen_shader(Some(missing)), SCREEN_SHADER);
    }
}
