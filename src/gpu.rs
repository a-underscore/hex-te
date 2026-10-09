use std::path::Path;
use std::sync::{Arc, RwLock};

use anyhow::anyhow;
use winit::window::Window;

use crate::WINDOW_TITLE;
use crate::drawable::Drawable;
use crate::terminal::Terminal;
use crate::world::{SystemManager, World};

/// The window, the surface, the device, and the [`Drawable`] that finishes every
/// frame.
///
/// The render command here owns the frame's own business — asking the surface
/// for an image, laying the terminal's grid out and presenting — and hands what
/// it has set up to the drawable, which runs the render systems and draws the
/// screen.
pub(crate) struct Gpu {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    drawable: Drawable,
}

impl Gpu {
    /// Opens the surface on `window` and builds the screen pipeline.
    ///
    /// `background` and `cursor_color` come from the config file, as `r, g, b, a`,
    /// `shader` is the WGSL source the config wrote, when it wrote one, and
    /// `background_image` is the picture it wants behind the grid.
    pub(crate) async fn new(
        window: Arc<Window>,
        background: [f32; 4],
        cursor_color: [f32; 4],
        shader: Option<&str>,
        background_image: Option<&Path>,
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

        // The drawable builds the screen pipeline the whole run draws with, so
        // a shader that does not compile is a failure to start.
        let mut drawable = Drawable::new(
            &device,
            &queue,
            config.format,
            background,
            cursor_color,
            shader,
            background_image,
        );
        drawable.set_resolution([config.width as f32, config.height as f32]);

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            drawable,
        })
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

        // The shader works a cell out from the resolution, so it follows the
        // surface. The uniform itself reaches the GPU on the next frame, which
        // is the frame this size belongs to.
        self.drawable.set_resolution([width as f32, height as f32]);
    }

    /// Draws one frame.
    ///
    /// The frame's own business is here — the image, the grid and presenting —
    /// and the last step is the [`Drawable`]'s: it runs the render systems the
    /// world carries and draws the screen over what is left of the image.
    pub(crate) fn render<E: 'static>(
        &mut self,
        terminal: &mut Terminal,
        world: Arc<RwLock<World<E>>>,
        sm: SystemManager<E>,
    ) -> anyhow::Result<()> {
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
            let replaced = terminal.update_layout(&*self, dimensions.width, dimensions.height);

            if replaced && let Some(texture) = terminal.texture.as_ref() {
                self.drawable.bind_texture(&self.device, texture);
            }
        }

        // The uniform says what the terminal looks like now: where the cursor
        // is, how big the grid is, and whether either is drawn at all.
        self.drawable.sync(&self.queue, terminal);
        self.drawable.draw(&mut encoder, &view, world, sm)?;

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);

        Ok(())
    }
}
