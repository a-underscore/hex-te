use std::sync::{Arc, Mutex, RwLock};

use nalgebra::Vector3;

use crate::world::{Control, EVENT_PIPELINE, Id, System, World};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, Event, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy},
    keyboard::{Key, ModifiersState},
    window::{Window, WindowId},
};

use crate::WINDOW_TITLE;
use crate::clipboard::Clipboard;
use crate::config::{Config, Reload};
use crate::pty::Pty;
use crate::render::{Gpu, Pictures};
use crate::terminal::font::Font;
use crate::terminal::{KeyModes, Terminal, encode_key};

/// Wakes the event loop back up when the shell has something to say.
#[derive(Debug)]
pub(crate) enum UserEvent {
    /// The shell wrote output that still has to be parsed and drawn.
    Output,
    /// The blink clock reached its next deadline; the phase may have flipped.
    Blink,
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
    /// The config file, watched so that an edit is picked up while the terminal
    /// runs rather than at the next start.
    reload: Reload,
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
            em.register::<Clipboard>();
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
        let before = world.read().unwrap().sm.read().unwrap().pipeline_counts();
        let config = Config::load(Arc::clone(&world));
        // What the file added to the world's pipelines, so that a reload can
        // take it back before running the file again.
        let reload = Reload::new(Config::path().ok(), &before, &world);

        world.read().unwrap().attach_value(entity, config);

        // The behaviour lives in the world too, which leaves `App` as nothing
        // but the winit glue around it.
        world
            .read()
            .unwrap()
            .add_system(0, TerminalSystem::new(entity, proxy));

        Ok(Self {
            world,
            entity,
            reload,
        })
    }

    /// Runs one event through the world's systems, stopping the loop if a
    /// system asked for it by setting [`Control::exit`].
    fn dispatch(&mut self, event_loop: &ActiveEventLoop, event: Event<UserEvent>) {
        let control = Control::new(event);
        let world = Arc::clone(&self.world);

        // A snapshot of the systems, taken out of the world's own lock: the
        // systems are free to reach back into the world while they run. Only
        // the event pipeline runs here — the render pipeline belongs to the
        // frame, and runs when one is drawn.
        let systems = world.read().unwrap().systems();

        if let Err(error) = systems.update_pipeline(EVENT_PIPELINE, Arc::clone(&control), world) {
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

    /// Re-reads the config file when it changed on disk, and puts what it says
    /// into the running app: the terminal's colours, and the renderer's pipeline,
    /// which is rebuilt from the shader and the pictures.
    fn reload_config(&mut self) {
        let Some(config) = self.reload.reload(Arc::clone(&self.world)) else {
            return;
        };

        if let Some(path) = self.reload.path() {
            eprintln!("{WINDOW_TITLE}: reloaded {}", path.display());
        }

        if let Some(terminal) = self.component::<Terminal>() {
            let mut terminal = terminal.write().unwrap();

            terminal.set_background(config.background);
            terminal.set_cursor_color(config.cursor_color);
        }

        if let Some(gpu) = self.component::<Gpu>() {
            let mut gpu = gpu.write().unwrap();

            gpu.apply(&config);
            // What the file changed is on screen at the next frame, whether or
            // not the config animates.
            gpu.request_redraw();
        }

        self.world
            .read()
            .unwrap()
            .attach_value(self.entity, config);
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
    /// Where the pointer is, in physical pixels, so that a drag can be turned
    /// into a cell without waiting for the next move.
    pointer: (f64, f64),
    /// Whether the left button is down, which is what makes a move a drag.
    dragging: bool,
    /// Used to wake the event loop up when the shell writes something.
    proxy: EventLoopProxy<UserEvent>,
}

impl TerminalSystem {
    fn new(entity: Id, proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            entity,
            pending: Arc::new(Mutex::new(Vec::new())),
            modifiers: ModifiersState::empty(),
            pointer: (0.0, 0.0),
            dragging: false,
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

    /// The cell the pointer is over, laid out the way the rasterizer lays the
    /// grid out, or nothing while there is no window to measure against.
    fn cell_under_pointer(&self, world: &World<UserEvent>) -> Option<(usize, usize)> {
        let surface = self.gpu(world)?.read().unwrap().size();
        let terminal = self.terminal(world)?;

        Some(terminal.read().unwrap().cell_at(self.pointer, surface))
    }

    /// Puts the selection on the clipboard. `Ctrl+Shift+C` is the shortcut every
    /// terminal uses for that, which is also why the key does not go on to the
    /// shell.
    fn copy_selection(&self, world: &World<UserEvent>) {
        let Some(text) = self
            .terminal(world)
            .and_then(|terminal| terminal.read().unwrap().selection_text())
        else {
            return;
        };

        let Some(clipboard) = world.component::<Clipboard>(self.entity) else {
            return;
        };

        if let Err(error) = clipboard.read().unwrap().copy(&text) {
            eprintln!("{WINDOW_TITLE}: {error:#}");
        }
    }

    fn request_redraw(&self, world: &World<UserEvent>) {
        if let Some(gpu) = self.gpu(world) {
            gpu.read().unwrap().request_redraw();
        }
    }

    /// Hands everything the reader thread collected to the VT parser, and
    /// writes back the answer if the shell asked the terminal a question.
    fn pump(&self, world: &World<UserEvent>) {
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        let output = std::mem::take(&mut *pending);
        drop(pending);

        let Some(terminal) = self.terminal(world) else {
            return;
        };

        let replies = {
            let mut terminal = terminal.write().unwrap();

            terminal.feed(&output);

            // `DSR` and a device attributes request are the shell asking where
            // the cursor is or what this terminal is; the answer goes back the
            // way a keystroke does.
            terminal.take_replies()
        };

        if !replies.is_empty()
            && let Some(pty) = self.pty(world)
            && let Err(error) = pty.read().unwrap().write(&replies)
        {
            eprintln!("{WINDOW_TITLE}: {error:#}");
        }
    }

    /// Draws one frame: the terminal's grid first, then the renderer, which
    /// finishes the frame with the world's own render systems.
    ///
    /// The world comes in by handle rather than by reference because those
    /// systems run inside the draw, and a system is always free to reach back
    /// into the world: no lock of the caller's may still be held by then.
    fn redraw(&self, world: Arc<RwLock<World<UserEvent>>>) {
        self.pump(&world.read().unwrap());

        let (gpu, terminal, systems) = {
            let world = world.read().unwrap();

            let (Some(gpu), Some(terminal)) = (
                world.component::<Gpu>(self.entity),
                world.component::<Terminal>(self.entity),
            ) else {
                return;
            };

            (gpu, terminal, world.systems())
        };

        if let Err(error) = gpu
            .write()
            .unwrap()
            .render(&terminal, Arc::clone(&world), systems)
        {
            eprintln!("{WINDOW_TITLE}: {error:#}");
        }

        // The grid size is only known once the renderer has laid it out, so the
        // shell learns about it after the first frame.
        let grid = terminal.read().unwrap().size;

        if let Some(pty) = world.read().unwrap().component::<Pty>(self.entity) {
            let _ = pty.read().unwrap().resize(grid.0, grid.1);
        }
    }

    /// Handles one window event, returning true when the loop should stop.
    fn window_event(&mut self, event: &WindowEvent, world: &World<UserEvent>) -> bool {
        match event {
            WindowEvent::CloseRequested => return true,
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            // Blinking stops while the window is in the background, so the
            // cursor is left solid and stays where it was left.
            WindowEvent::Focused(focused) => {
                if let Some(terminal) = self.terminal(world) {
                    terminal.write().unwrap().set_focused(*focused);
                }

                // An animated shader pauses with the blink clock, so the
                // renderer is told as well.
                if let Some(gpu) = self.gpu(world) {
                    gpu.write().unwrap().set_focused(*focused);
                }

                self.request_redraw(world);
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu(world) {
                    gpu.write().unwrap().resize(size.width, size.height);
                }

                self.request_redraw(world);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x, position.y);

                // A move is only a drag while the button is down; without it
                // the pointer is just being remembered for the next one.
                if self.dragging
                    && let Some(cell) = self.cell_under_pointer(world)
                {
                    if let Some(terminal) = self.terminal(world) {
                        terminal.write().unwrap().extend_selection(cell);
                    }

                    self.request_redraw(world);
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                // Pressing starts a new selection at the cell under the
                // pointer, however short it turns out to be; releasing just
                // ends the drag, so what was selected stays selected.
                if *state == ElementState::Pressed {
                    self.dragging = true;

                    if let Some(cell) = self.cell_under_pointer(world)
                        && let Some(terminal) = self.terminal(world)
                    {
                        terminal.write().unwrap().start_selection(cell);
                    }
                } else {
                    self.dragging = false;
                }

                self.request_redraw(world);
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && matches!(&event.logical_key, Key::Character(key)
                        if key.eq_ignore_ascii_case("c")) =>
            {
                self.copy_selection(world);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // The cursor shows again while keys are arriving, the way a
                // terminal stops blinking when it is typed into. What the shell
                // has asked of the keyboard comes out of the same borrow.
                let modes = match self.terminal(world) {
                    Some(terminal) => {
                        let mut terminal = terminal.write().unwrap();

                        terminal.wake_blink();
                        terminal.key_modes()
                    }
                    None => KeyModes::default(),
                };

                if let Some(bytes) = encode_key(&event.logical_key, self.modifiers, modes)
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
        world.attach_value(
            self.entity,
            Terminal::new(font, config.background, config.cursor_color),
        );

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

        // A clipboard the session will not give up is not worth refusing to
        // start over: selecting still works, and only a copy is missing.
        match Clipboard::new() {
            Ok(clipboard) => {
                world.attach_value(self.entity, clipboard);
            }
            Err(error) => eprintln!("{WINDOW_TITLE}: {error:#}"),
        }

        Ok(())
    }

    fn update(
        &mut self,
        control: Arc<RwLock<Control<UserEvent>>>,
        world: Arc<RwLock<World<UserEvent>>>,
    ) -> anyhow::Result<()> {
        let mut exit = false;
        let mut draw = false;

        {
            let control = control.read().unwrap();
            let world = world.read().unwrap();

            match &control.event {
                // A frame is drawn with the world's lock free, so the render
                // systems it runs are free to reach back into the world. Every
                // other event is handled with it held, as before.
                Event::WindowEvent {
                    event: WindowEvent::RedrawRequested,
                    ..
                } => draw = true,
                Event::WindowEvent { event, .. } => exit = self.window_event(event, &world),
                Event::UserEvent(UserEvent::Output) => {
                    self.pump(&world);
                    self.request_redraw(&world);
                }
                Event::UserEvent(UserEvent::Blink) => {
                    // Only redraw when the phase really moved: this event is
                    // dispatched on every wake-up, deadlines included.
                    if let Some(terminal) = self.terminal(&world)
                        && terminal.write().unwrap().tick_blink()
                    {
                        self.request_redraw(&world);
                    }
                }
                Event::UserEvent(UserEvent::Closed) => exit = true,
                _ => {}
            }
        }

        if exit {
            control.write().unwrap().exit = true;
        }

        if draw {
            self.redraw(Arc::clone(&world));
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

        let gpu = match pollster::block_on(Gpu::new(
            window,
            config.shader.as_deref(),
            Pictures {
                background: config.background_image.as_deref(),
                foreground: config.foreground_image.as_deref(),
            },
            config.animate,
        )) {
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // The blink clock is driven through the systems like every other event;
        // this is the one event the app makes up itself.
        self.dispatch(event_loop, Event::UserEvent(UserEvent::Blink));

        // A file that changed is re-read here, on the turn where the loop is
        // about to sleep or draw: the config is not something that has to be
        // noticed within a frame.
        self.reload_config();

        // Then sleep until the clock's next flip, so a quiet shell costs
        // nothing at all. `Wait` is enough once nothing on screen blinks.
        let deadline = self
            .component::<Terminal>()
            .and_then(|terminal| terminal.read().unwrap().next_blink());
        // An animated shader wants a frame every frame. Asking for the next one
        // here keeps the loop awake without spinning: presenting a frame still
        // paces them to the display, and nothing is asked for while the window
        // is in the background.
        if let Some(gpu) = self.component::<Gpu>() {
            let gpu = gpu.read().unwrap();

            if gpu.animating() {
                gpu.request_redraw();
            }
        }

        event_loop.set_control_flow(match deadline {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
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
