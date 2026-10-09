use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::anyhow;
use winit::dpi::LogicalSize;
use winit::window::Window;

use crate::WINDOW_TITLE;
use crate::config::Config;
use crate::render::{Drawable, Pictures};
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
    /// Whether the config asked for a frame every frame, so that a shader with a
    /// clock in it can move. Off by default: the app otherwise draws only when
    /// something changed.
    animate: bool,
    /// Whether the window has the focus. An animation is paused while it does
    /// not, the way the blink clock is, so a buried terminal costs nothing.
    focused: bool,
    /// The instant a shader's `time` is counted from. Made once, when the app
    /// starts, so that rebuilding the pipeline for a reloaded config does not
    /// restart the clock a shader is reading.
    start: Instant,
}

impl Gpu {
    /// Opens the surface on `window` and builds the screen pipeline.
    ///
    /// `shader` is the WGSL source the config wrote, when it wrote one,
    /// `pictures` are the pictures it named, when it named any, and `animate` is
    /// whether it asked for a frame every frame.
    pub(crate) async fn new(
        window: Arc<Window>,
        shader: Option<&str>,
        pictures: Pictures<'_>,
        animate: bool,
    ) -> anyhow::Result<Self> {
        // The clock a shader's `time` counts from, made once for the whole run.
        let start = Instant::now();

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
            shader,
            pictures,
            start,
        );
        drawable.set_resolution([config.width as f32, config.height as f32]);

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            drawable,
            animate,
            focused: true,
            start,
        })
    }

    /// Takes a config the app reloaded: the shader and the pictures are rebuilt
    /// from it, the flag that keeps the loop awake follows it, and the window is
    /// asked for the size it names.
    ///
    /// The colours are not here: they live on the terminal, which is what a
    /// frame reads them from, and the app puts the reloaded ones there.
    pub(crate) fn apply(&mut self, config: &Config) {
        self.animate = config.animate;
        self.drawable = Drawable::new(
            &self.device,
            &self.queue,
            self.config.format,
            config.shader.as_deref(),
            Pictures {
                background: config.background_image.as_deref(),
                foreground: config.foreground_image.as_deref(),
            },
            self.start,
        );
        self.drawable
            .set_resolution([self.config.width as f32, self.config.height as f32]);

        let (width, height) = config.window_size;

        // A window manager is free to say no, which is why the result is only
        // dropped: the size is a request, as it is at startup.
        let _ = self
            .window
            .request_inner_size(LogicalSize::new(width, height));
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub(crate) fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Whether a frame should be drawn as soon as the loop goes idle: the config
    /// asked for animation and the window is focused. This is what keeps a
    /// shader's clock ticking while nothing else is happening.
    pub(crate) fn animating(&self) -> bool {
        self.animate && self.focused
    }

    /// Remembers whether the window has the focus, so an animation pauses behind
    /// another window instead of drawing frames nobody will see.
    pub(crate) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The size of the window in physical pixels: what the grid is laid out in,
    /// and so what a pointer position is measured against.
    pub(crate) fn size(&self) -> (u32, u32) {
        let size = self.window.inner_size();

        (size.width, size.height)
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
    ///
    /// The terminal is handed over as a handle rather than as a borrow, because
    /// the render systems below are handed the world, and a config file is free
    /// to reach the terminal from one of them. Holding it locked while they run
    /// would deadlock the first render function that did.
    pub(crate) fn render<E: 'static>(
        &mut self,
        terminal: &Arc<RwLock<Terminal>>,
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
            let mut terminal = terminal.write().unwrap();
            let replaced = terminal.update_layout(&*self, dimensions.width, dimensions.height);

            if replaced && let Some(texture) = terminal.texture.as_ref() {
                self.drawable.bind_texture(&self.device, texture);
            }
        }

        // The uniform says what the terminal looks like now: the colours the
        // frame composites with, where the cursor is, how big the grid is, and
        // whether either is drawn at all. The lock is held for the copy and no
        // longer, so a render system below can reach the terminal itself.
        {
            let terminal = terminal.read().unwrap();

            self.drawable.sync(&self.queue, &terminal);
        }

        self.drawable.draw(&mut encoder, &view, world, sm)?;

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);

        Ok(())
    }
}
