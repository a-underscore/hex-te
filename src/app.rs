use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

use crate::gpu::Gpu;
use crate::pty::Pty;
use crate::{WINDOW_TITLE, terminal::Terminal};

const INITIAL_WIDTH: u32 = 1024;
const INITIAL_HEIGHT: u32 = 640;

pub(crate) struct App {
    pub gpu: Option<Gpu>,
    pub terminal: Option<Terminal>,
    pub pty: Pty,
}

impl App {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            pty: Pty::new()?,
            gpu: None,
            terminal: Some(Terminal::default()),
        })
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_inner_size(LogicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error}");
                event_loop.exit();
                return;
            }
        };

        self.terminal = Some(Terminal::default());

        match pollster::block_on(Gpu::new(window)) {
            Ok(gpu) => {
                gpu.request_redraw();
                self.gpu = Some(gpu);
            }
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(gpu) = self.gpu.as_mut() else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => gpu.resize(size.width, size.height),
            WindowEvent::RedrawRequested => {
                let Some(terminal) = self.terminal.as_mut() else {
                    return;
                };

                if let Err(error) = gpu.render(terminal) {
                    eprintln!("{WINDOW_TITLE}: {error:#}");
                }
            }
            _ => {}
        }
    }
}
