use std::sync::{Arc, Mutex, RwLock};

use nalgebra::Vector3;

use crate::control::Control;
use crate::id::Id;
use crate::world::{System, World};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, Event, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::ModifiersState,
    window::{Window, WindowId},
};

use crate::WINDOW_TITLE;
use crate::config::Config;
use crate::font::Font;
use crate::gpu::Gpu;
use crate::pty::Pty;
use crate::terminal::{Terminal, encode_key};

/// Wakes the event loop back up when the shell has something to say.
#[derive(Debug)]
pub(crate) enum UserEvent {
    /// The shell wrote output that still has to be parsed and drawn.
    Output,
    /// The shell exited, so there is nothing left to show.
    Closed,
}

/// The terminal application.
///
/// Everything it needs lives in a [`World`]: `Gpu`, `Font`, `Terminal` and `Pty`
/// are components of one entity, and the behaviour is a [`System`] in the
/// world's own manager that borrows them as it runs. `App` itself is only the
/// winit glue that turns events into [`Control`] values.
pub(crate) struct App {
    world: Arc<RwLock<World<UserEvent>>>,
    entity: Id,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<Self> {
        // The engine's world also carries the ambient lighting values the 3D
        // renderer reads; a terminal has no lights, so they stay at zero.
        let world = World::<UserEvent>::new(Vector3::zeros(), 0.0);
        let entity = world.read().unwrap().spawn(true);

        {
            let world = world.read().unwrap();
            let mut em = world.em.write().unwrap();

            // Register the managers up front so the world knows about the app's
            // component types before the first `attach`.
            em.register::<Config>();
            em.register::<Gpu>();
            em.register::<Font>();
            em.register::<Terminal>();
            em.register::<Pty>();
        }

        // Read before the window exists, because the config sizes it; this is
        // also what writes the documented default file on first run. It is
        // loaded outside the `read` guard on purpose: the config is handed the
        // world, and a `config.py` that touched it would deadlock against a
        // guard held here.
        let config = Config::load(Arc::clone(&world));

        world.read().unwrap().attach_value(entity, config);

        // The behaviour lives in the world too, which leaves `App` as nothing
        // but the winit glue around it.
        world
            .read()
            .unwrap()
            .add_system(0, TerminalSystem::new(entity, proxy));

        Ok(Self { world, entity })
    }

    /// Runs one event through the world's systems, stopping the loop if a
    /// system asked for it by setting [`Control::exit`].
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, event: Event<UserEvent>) {
        let control = Control::new(event);
        let world = Arc::clone(&self.world);

        // A snapshot of the systems, taken out of the world's own lock: the
        // systems are free to reach back into the world while they run.
        let systems = world.read().unwrap().systems();

        if let Err(error) = systems.update(Arc::clone(&control), world) {
            eprintln!("{WINDOW_TITLE}: {error:#}");
        }

        if control.read().unwrap().exit {
            event_loop.exit();
        }
    }

    /// The component of type `C` attached to the app's entity.
    fn component<C: Send + Sync + 'static>(&self) -> Option<Arc<RwLock<C>>> {
        self.world.read().unwrap().component::<C>(self.entity)
    }

    /// The config, or the built-in defaults if it is somehow missing.
    fn config(&self) -> Config {
        self.component::<Config>()
            .map(|config| config.read().unwrap().clone())
            .unwrap_or_default()
    }
}

/// The terminal's behaviour: a single system that handles every event by
/// borrowing the app's components out of the world.
struct TerminalSystem {
    /// The entity the components are attached to.
    entity: Id,
    /// Output the reader thread collected that the grid has not parsed yet.
    pending: Arc<Mutex<Vec<u8>>>,
    modifiers: ModifiersState,
    /// Used to wake the event loop up when the shell writes something.
    proxy: EventLoopProxy<UserEvent>,
}

impl TerminalSystem {
    fn new(entity: Id, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            entity,
            pending: Arc::new(Mutex::new(Vec::new())),
            modifiers: ModifiersState::empty(),
            proxy,
        }
    }

    fn gpu(&self, world: &World<UserEvent>) -> Option<Arc<RwLock<Gpu>>> {
        world.component::<Gpu>(self.entity)
    }

    fn terminal(&self, world: &World<UserEvent>) -> Option<Arc<RwLock<Terminal>>> {
        world.component::<Terminal>(self.entity)
    }

    fn pty(&self, world: &World<UserEvent>) -> Option<Arc<RwLock<Pty>>> {
        world.component::<Pty>(self.entity)
    }

    fn request_redraw(&self, world: &World<UserEvent>) {
        if let Some(gpu) = self.gpu(world) {
            gpu.read().unwrap().request_redraw();
        }
    }

    /// Hands everything the reader thread collected to the VT parser.
    fn pump(&self, world: &World<UserEvent>) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        let output = std::mem::take(&mut *pending);
        drop(pending);

        if let Some(terminal) = self.terminal(world) {
            terminal.write().unwrap().feed(&output);
        }
    }

    fn redraw(&self, world: &World<UserEvent>) {
        self.pump(world);

        let (Some(gpu), Some(terminal)) = (self.gpu(world), self.terminal(world)) else {
            return;
        };

        if let Err(error) = gpu.write().unwrap().render(&mut terminal.write().unwrap()) {
            eprintln!("{WINDOW_TITLE}: {error:#}");
        }

        // The grid size is only known once the renderer has laid it out, so the
        // shell learns about it after the first frame.
        let grid = terminal.read().unwrap().size;

        if let Some(pty) = self.pty(world) {
            let _ = pty.read().unwrap().resize(grid.0, grid.1);
        }
    }

    /// Handles one window event, returning true when the loop should stop.
    fn window_event(&mut self, event: &WindowEvent, world: &World<UserEvent>) -> bool {
        match event {
            WindowEvent::CloseRequested => return true,
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu(world) {
                    gpu.write().unwrap().resize(size.width, size.height);
                }

                self.request_redraw(world);
            }
            WindowEvent::RedrawRequested => self.redraw(world),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(bytes) = encode_key(&event.logical_key, self.modifiers)
                    && let Some(pty) = self.pty(world)
                    && let Err(error) = pty.read().unwrap().write(&bytes)
                {
                    eprintln!("{WINDOW_TITLE}: {error:#}");
                }

                // The shell echoes what it received, so the grid catches up
                // once the reader thread reports the output.
                self.request_redraw(world);
            }
            _ => {}
        }

        false
    }
}

impl System<UserEvent> for TerminalSystem {
    fn init(&mut self, world: Arc<RwLock<World<UserEvent>>>) -> anyhow::Result<()> {
        let world = world.read().unwrap();

        let config = world
            .component::<Config>(self.entity)
            .map(|config| config.read().unwrap().clone())
            .unwrap_or_default();

        let font = world.attach_value(self.entity, Font::load(config.font_size)?);
        world.attach_value(self.entity, Terminal::new(font, config.background));

        // Reads from the pty block, so they happen on the reader thread and the
        // result is handed to the event loop as a user event.
        let sink = Arc::clone(&self.pending);
        let proxy = self.proxy.clone();
        let pty = Pty::new(config.shell.as_deref(), move |chunk| match chunk {
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
        world.attach_value(self.entity, pty);

        Ok(())
    }

    fn update(
        &mut self,
        control: Arc<RwLock<Control<UserEvent>>>,
        world: Arc<RwLock<World<UserEvent>>>,
    ) -> anyhow::Result<()> {
        let mut exit = false;

        {
            let control = control.read().unwrap();
            let world = world.read().unwrap();

            match &control.event {
                Event::WindowEvent { event, .. } => exit = self.window_event(event, &world),
                Event::UserEvent(UserEvent::Output) => {
                    self.pump(&world);
                    self.request_redraw(&world);
                }
                Event::UserEvent(UserEvent::Closed) => exit = true,
                _ => {}
            }
        }

        if exit {
            control.write().unwrap().exit = true;
        }

        Ok(())
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.component::<Gpu>().is_some() {
            return;
        }

        let config = self.config();

        let attributes = Window::default_attributes()
            .with_title(WINDOW_TITLE)
            .with_inner_size(LogicalSize::new(config.window_size.0, config.window_size.1));

        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error}");
                event_loop.exit();
                return;
            }
        };

        let gpu = match pollster::block_on(Gpu::new(window, config.background, config.cursor_color))
        {
            Ok(gpu) => gpu,
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error:#}");
                event_loop.exit();
                return;
            }
        };

        gpu.request_redraw();
        self.world.read().unwrap().attach_value(self.entity, gpu);

        // The window has to exist before the rest of the app can be built, so
        // the systems are initialised here rather than in `App::new`.
        let systems = self.world.read().unwrap().systems();

        if let Err(error) = systems.init(Arc::clone(&self.world)) {
            eprintln!("{WINDOW_TITLE}: {error:#}");
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.dispatch(event_loop, Event::UserEvent(event));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.dispatch(event_loop, Event::WindowEvent { window_id, event });
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // Take the shell down with the window instead of orphaning it.
        if let Some(pty) = self.component::<Pty>() {
            let _ = pty.read().unwrap().kill();
        }
    }
}
