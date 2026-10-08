use std::sync::{Arc, Mutex};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Window, WindowId},
};

use crate::WINDOW_TITLE;
use crate::gpu::Gpu;
use crate::pty::Pty;
use crate::terminal::{Terminal, encode_key};

const INITIAL_WIDTH: u32 = 1024;
const INITIAL_HEIGHT: u32 = 640;

/// Wakes the event loop back up when the shell has something to say.
#[derive(Debug)]
pub(crate) enum UserEvent {
    /// The shell wrote output that still has to be parsed and drawn.
    Output,
    /// The shell exited, so there is nothing left to show.
    Closed,
}

pub(crate) struct App {
    pub gpu: Option<Gpu>,
    pub terminal: Option<Terminal>,
    pub pty: Pty,
    /// Output the reader thread collected that the grid has not parsed yet.
    pending: Arc<Mutex<Vec<u8>>>,
    modifiers: ModifiersState,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<Self> {
        let pending = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&pending);

        // Reads from the pty block, so they happen on the reader thread and the
        // result is handed to the event loop as a user event.
        let pty = Pty::new(move |chunk| match chunk {
            Some(bytes) => {
                if let Ok(mut collected) = sink.lock() {
                    collected.extend_from_slice(bytes);
                }

                let _ = proxy.send_event(UserEvent::Output);
            }
            None => {
                let _ = proxy.send_event(UserEvent::Closed);
            }
        })?;

        Ok(Self {
            pty,
            gpu: None,
            terminal: Some(Terminal::new()?),
            pending,
            modifiers: ModifiersState::empty(),
        })
    }

    /// Hands everything the reader thread collected to the VT parser.
    fn pump_output(&mut self) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        let output = std::mem::take(&mut *pending);

        if let Some(terminal) = self.terminal.as_mut() {
            terminal.feed(&output);
        }
    }

    fn request_redraw(&self) {
        if let Some(gpu) = self.gpu.as_ref() {
            gpu.request_redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
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

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Output => {
                self.pump_output();
                self.request_redraw();
            }
            UserEvent::Closed => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }

                self.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                self.pump_output();

                let Some(terminal) = self.terminal.as_mut() else {
                    return;
                };

                if let Some(gpu) = self.gpu.as_mut() {
                    if let Err(error) = gpu.render(terminal) {
                        eprintln!("{WINDOW_TITLE}: {error:#}");
                    }

                    // The grid size is only known once the renderer has laid it
                    // out, so the shell learns about it after the first frame.
                    let _ = self.pty.resize(terminal.size.0, terminal.size.1);
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(bytes) = encode_key(&event.logical_key, self.modifiers) {
                    if let Err(error) = self.pty.write(&bytes) {
                        eprintln!("{WINDOW_TITLE}: {error:#}");
                    }
                }

                // The shell echoes what it received, so the grid catches up
                // once the reader thread reports the output.
                self.request_redraw();
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Take the shell down with the window instead of orphaning it.
        let _ = self.pty.child.kill();
    }
}
