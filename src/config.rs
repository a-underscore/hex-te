//! The configuration file: a Python module in the user's config directory,
//! evaluated with an embedded interpreter.
//!
//! On first run the app writes a documented `config.py` there and reads it back;
//! every setting is optional and falls back to the default the file documents.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use nalgebra::Vector3;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::WINDOW_TITLE;
use crate::app::UserEvent;
use crate::terminal::Terminal;
use crate::world::{Control, Id, RENDER_PIPELINE, System, World};

/// The settings the app reads, and what it uses when the file does not say.
#[derive(Clone)]
pub(crate) struct Config {
    /// Glyphs are rasterized at this size, in logical pixels.
    pub font_size: f32,
    /// Size of a new window, in logical pixels.
    pub window_size: (u32, u32),
    /// Program to run inside the pty; `None` means `$SHELL`.
    pub shell: Option<String>,
    /// Background colour, as `r, g, b, a`.
    pub background: [f32; 4],
    /// Cursor colour, as `r, g, b, a`.
    pub cursor_color: [f32; 4],
    /// Whether to draw a frame every frame, so a shader with a clock in it can
    /// move. `false` draws only when something changed, which is the default.
    pub animate: bool,
    /// The WGSL the screen pipeline is built from, as source. `None` keeps the
    /// shader compiled into the binary, which is what the app ships with.
    pub shader: Option<String>,
    /// A picture to draw behind the grid. `None` draws the plain background
    /// colour, which is what the shader does when no picture is there at all.
    pub background_image: Option<PathBuf>,
    /// A picture to draw over everything, transparent where the screen below
    /// should show through. `None` is no overlay at all.
    pub foreground_image: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            window_size: (1024, 640),
            shell: None,
            background: [0.05, 0.06, 0.08, 1.0],
            cursor_color: [0.16, 0.72, 0.72, 1.0],
            animate: false,
            shader: None,
            background_image: None,
            foreground_image: None,
        }
    }
}

/// Written on first run. Since the loader keeps its default for anything missing
/// or invalid, this doubles as the documentation for the file.
const DEFAULT_CONFIG: &str = r#"# hext configuration.
#
# This file is Python: the host evaluates it with an embedded interpreter, so
# anything that produces the right value works. Every name is optional, and one
# that is missing or of the wrong type falls back to the default shown here.
#
# The app reads this file at startup and then watches it, so an edit and a save
# changes the running terminal — the colours, the shader, the pictures, whether
# it animates — without a restart. A file that cannot be evaluated, or a setting
# of the wrong type, is reported and that setting keeps its default; nothing in
# this file can stop the terminal starting.

# Glyphs are rasterized at this size, in logical pixels.
font_size = 16.0

# Size of a new window, in logical pixels.
window_width = 1024
window_height = 640

# Program to run inside the pty. `None` means $SHELL.
shell = None

# Colours, as (r, g, b) floats in 0.0..=1.0.
background = (0.05, 0.06, 0.08)
cursor_color = (0.16, 0.72, 0.72)

# The screen shader, as WGSL source: the shader the terminal draws with is part
# of this file. It needs the `vs_screen` and `fs_screen` entry points and the
# bindings `src/render/shaders/screen.wgsl` uses, which is also what `None` falls
# back to. Because this is Python, a file can be read instead of pasted:
#
#     shader = open("/home/you/.config/hext/crt.wgsl").read()
shader = None

# Whether to draw a frame every frame. `False` draws only when something has
# changed — a keystroke, output from the shell, the blink clock — which is what
# the app does by default and what makes a quiet terminal cost nothing. `True`
# draws continuously, at whatever rate the display and the GPU settle on, which
# is what a shader that moves needs: the uniform it reads ends with a `time`
# field (seconds since the app started), and without this a shader only ever
# sees its first frame. Nothing is drawn while the window is in the background,
# so an animation pauses there rather than running unseen.
#
#     animate = True
animate = False

# A picture to draw behind the grid, as the path to a PNG or a JPEG. The screen
# is ink rather than a picture — its alpha says how much of a pixel each cell
# covers — so the picture shows wherever a program left a cell unpainted:
# Neovim's `hi Normal guibg=NONE ctermbg=NONE` (without it Neovim paints a
# background on every cell, which covers the picture), or a shell that only
# writes text. Anything that asked for a colour of its own stays opaque on top.
#
#     background_image = "/home/you/pictures/terminal.png"
#
# `None` shows the plain `background` colour instead, and a picture that cannot
# be read is reported and the colour used.
background_image = None

# A picture to draw over everything, stretched the same way: the glass in front
# of the tube. Where it is transparent the screen shows through, so a shadow
# mask, a grille, a sheet of glare or a scratch is a PNG with an alpha channel:
#
#     foreground_image = "/home/you/pictures/mask.png"
#
# `None` is no overlay at all, and a picture that cannot be read is reported and
# nothing is drawn over the screen.
foreground_image = None

# The app's entity-component world is bound here as `world`: the same object the
# engine goes on using, so this file can seed it and set it up. It holds the
# entities, and the ambient values a 3D pass starts from:
#
#   world.spawn(active=True) -> int    # add an entity, returning its id
#   world.despawn(entity)              # remove it again
#   world.entities() -> [int]          # the ids of the active entities
#   world.entity_count() -> int
#   world.ambient_color = (r, g, b)    # base colour of the 3D lighting pass
#   world.ambient_intensity = 1.0
#
# The world also holds the systems, which is how this file adds behaviour of its
# own. A system is an ordinary function taking the world; `pipeline` says when
# it is called:
#
#   world.add_system(fn, pipeline=0)                 # fn(world), every event
#   world.add_system(fn, pipeline=render_pipeline)   # fn(world), every frame
#   world.remove_system(pipeline=0)                  # drops the most recent one
#   world.system_count() -> int
#
# Pipeline 0 is the app's own: the function runs once per event the event loop
# dispatches. `render_pipeline` is the frame — the function runs once per frame,
# just before the screen is drawn over the image — which is how a render
# function is written here:
#
#   frames = 0
#
#   def draw(world):
#       global frames
#       frames += 1
#
#   world.add_system(draw, pipeline=render_pipeline)
#
# Both kinds run on the event loop's thread, with none of the app's own locks
# held, so a system may read and write the world it is handed — including
# registering further systems, which join the next event or the next frame. A
# frame is not an event, so a render function is handed the event that says the
# loop has nothing left to do.
#
# A render function also reaches the terminal the frame is drawn from:
#
#   terminal = world.terminal()        # None until the app has built it
#   terminal.cursor                    # (column, row), read-only
#   terminal.size                      # (columns, rows), read-only
#   terminal.cursor_visible            # read-only
#   terminal.cursor_style              # 0 block, 1 bar, 2 underline, read-only
#   terminal.cursor_size               # (width, height) of one cell, read-only
#   terminal.background = (r, g, b)    # the colour behind the grid
#   terminal.cursor_color = (r, g, b)  # the colour the cursor is drawn in
#
# The two colours are read out of the terminal on every frame, so writing them
# from a render function animates them, which is the other way a config moves:
#
#   import math, time
#
#   started = time.monotonic()
#
#   def drift(world):
#       phase = time.monotonic() - started
#       terminal = world.terminal()
#       if terminal is not None:
#           terminal.cursor_color = (0.16, 0.72, 0.5 + 0.5 * math.sin(phase))
#
#   world.add_system(drift, pipeline=render_pipeline)
#
# A change made there lands on the next frame, because a frame's uniform is
# written before its render functions run — a frame the eye cannot see.
#
# Systems are the one thing a reload has to be careful with: the file is
# evaluated again when it changes, so the systems it added are taken back first
# and added afresh. A reloaded file therefore replaces the previous one's
# behaviour rather than stacking on top of it, and anything it spawned or
# changed in the world stays as it was.
"#;

impl Config {
    /// Where the config lives: `$XDG_CONFIG_HOME/hext/config.py`, or
    /// `$HOME/.config/hext/config.py` when `XDG_CONFIG_HOME` is unset.
    pub fn path() -> anyhow::Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(
                std::env::var_os("HOME")
                    .ok_or_else(|| anyhow::anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))?,
            )
            .join(".config"),
        };

        Ok(base.join("hext").join("config.py"))
    }

    /// Reads the config, creating it from [`DEFAULT_CONFIG`] if it is not there
    /// yet.
    ///
    /// Never fails: a config that cannot be found or evaluated is reported and
    /// the defaults are used, so a broken file cannot stop the terminal starting.
    pub fn load(world: Arc<RwLock<World<UserEvent>>>) -> Self {
        let path = match Self::path() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error}");
                return Self::default();
            }
        };

        match Self::read_from(&path, world) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error:#}");
                Self::default()
            }
        }
    }

    /// Creates `path` if it does not exist, then evaluates it.
    pub fn read_from(path: &Path, world: Arc<RwLock<World<UserEvent>>>) -> anyhow::Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::write(path, DEFAULT_CONFIG)?;
        }

        Self::evaluate(&std::fs::read_to_string(path)?, world)
    }

    /// Runs `source` as a Python module and reads the settings out of its
    /// globals.
    ///
    /// The world is bound in the module as `world`, so a config can reach the
    /// entity store and the ambient lighting values through the handle the rest
    /// of the app is already using.
    fn evaluate(source: &str, world: Arc<RwLock<World<UserEvent>>>) -> anyhow::Result<Self> {
        let source = CString::new(source)?;
        let mut config = Self::default();

        Python::attach(|py| -> PyResult<()> {
            let globals = PyDict::new(py);

            globals.set_item("world", Py::new(py, PyWorld { world })?)?;
            // The pipeline the renderer runs, so a render function can be
            // registered by name rather than by number.
            globals.set_item("render_pipeline", RENDER_PIPELINE)?;

            py.run(source.as_c_str(), Some(&globals), None)?;

            config.shader = setting_text(&globals, "shader");
            config.background_image = setting_path(&globals, "background_image");
            config.foreground_image = setting_path(&globals, "foreground_image");
            if let Some(animate) = setting_bool(&globals, "animate") {
                config.animate = animate;
            }

            Ok(())
        })?;

        Ok(config)
    }
}

/// Reads a setting that holds a path.
///
/// A picture that cannot be opened is reported by whoever loads it, so all this
/// does is take the name: an empty one is the same as not naming a picture at
/// all.
fn setting_path(globals: &Bound<'_, PyDict>, name: &str) -> Option<PathBuf> {
    setting_text(globals, name)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// Reads a setting that holds text.
///
/// A setting that is absent, `None`, or not a string keeps the default the
/// config was built with: one bad setting should not stop the rest of the file
/// working, so the problem is reported and the default kept.
fn setting_text(globals: &Bound<'_, PyDict>, name: &str) -> Option<String> {
    let value = match globals.get_item(name) {
        Ok(Some(value)) if !value.is_none() => value,
        Ok(_) => return None,
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: {name}: {error}");

            return None;
        }
    };

    match value.extract::<String>() {
        Ok(text) => Some(text),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: {name} is not text: {error}");

            None
        }
    }
}

/// Reads a setting that holds a flag.
///
/// The flag has to be a Python `bool`: `False` is a value and is taken, so it is
/// only an absent setting, `None` or the wrong type that keeps the default the
/// config was built with.
fn setting_bool(globals: &Bound<'_, PyDict>, name: &str) -> Option<bool> {
    let value = match globals.get_item(name) {
        Ok(Some(value)) if !value.is_none() => value,
        Ok(_) => return None,
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: {name}: {error}");

            return None;
        }
    };

    match value.extract::<bool>() {
        Ok(flag) => Some(flag),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: {name} is not a flag: {error}");

            None
        }
    }
}

/// How often the config file is looked at, so that a loop drawing sixty frames a
/// second does not read it sixty times a second.
const POLL: Duration = Duration::from_millis(200);

/// The config file, watched for changes while the terminal runs.
///
/// The app keeps one of these and asks it, on every turn of the event loop,
/// whether the file says something new. When it does the file is evaluated
/// again and the result handed back to be put into the running terminal, so an
/// edit and a save is all it takes to change the colours, the shader, the
/// pictures or the animation — no restart.
pub(crate) struct Reload {
    /// Where the config is. `None` when the app could not work out a path at
    /// all, which is when there is nothing to watch.
    path: Option<PathBuf>,
    /// The text the config was last built from, so a poll can tell whether
    /// anything really changed.
    source: String,
    /// When the file was last read, which is what keeps the polls to a few a
    /// second.
    checked: Instant,
    /// What the file added to each pipeline when it was last evaluated: a reload
    /// takes those back before the file runs again, so saving twice does not
    /// leave the first save's systems behind.
    added: HashMap<Id, usize>,
}

impl Reload {
    /// Starts watching the config that was just loaded.
    ///
    /// `before` counts the world's systems from before that load, so the ones
    /// the file added can be told from the app's own.
    pub(crate) fn new(
        path: Option<PathBuf>,
        before: &HashMap<Id, usize>,
        world: &Arc<RwLock<World<UserEvent>>>,
    ) -> Self {
        let source = path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();

        Self {
            path,
            source,
            checked: Instant::now(),
            added: added_systems(before, &counts(world)),
        }
    }

    /// The config a changed file holds, or `None` while there is nothing new.
    ///
    /// The file is read at most every [`POLL`], and only text that really
    /// changed is evaluated: saving a file without editing it is nothing at all.
    ///
    /// A file that cannot be evaluated is reported and leaves the running
    /// settings as they were. Its systems have been taken back by then, so a
    /// broken file keeps the look and drops only the behaviour the last good one
    /// added.
    pub(crate) fn reload(&mut self, world: Arc<RwLock<World<UserEvent>>>) -> Option<Config> {
        if self.checked.elapsed() < POLL {
            return None;
        }

        self.checked = Instant::now();

        let source = std::fs::read_to_string(self.path.as_deref()?).ok()?;

        if source == self.source {
            return None;
        }

        self.source = source;

        // Whatever the last evaluation added goes first: the file's effects
        // should be the file, not the file once per save.
        {
            let world = world.read().unwrap();
            let mut systems = world.sm.write().unwrap();

            for (&pipeline, &count) in &self.added {
                for _ in 0..count {
                    systems.rm(pipeline);
                }
            }
        }

        let before = counts(&world);
        let config = Config::evaluate(&self.source, Arc::clone(&world));

        self.added = added_systems(&before, &counts(&world));

        match config {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: reloading the config: {error:#}");

                None
            }
        }
    }

    /// The file being watched, for the note the app prints when it reloads.
    pub(crate) fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// How many systems each pipeline holds.
fn counts(world: &Arc<RwLock<World<UserEvent>>>) -> HashMap<Id, usize> {
    world.read().unwrap().sm.read().unwrap().pipeline_counts()
}

/// How many systems each pipeline gained between two counts.
fn added_systems(before: &HashMap<Id, usize>, after: &HashMap<Id, usize>) -> HashMap<Id, usize> {
    after
        .iter()
        .filter_map(|(pipeline, count)| {
            let before = before.get(pipeline).copied().unwrap_or(0);

            (count > &before).then_some((*pipeline, count - before))
        })
        .collect()
}

/// The app's [`World`], as the config file sees it.
///
/// A Rust value has to be a `#[pyclass]` before Python can hold it, and this is
/// the wrapper that lets `config.py` read and change the world the app is
/// building. It holds the same [`Arc`] the rest of the app does, so nothing is
/// copied: a change here is visible to the engine immediately.
#[pyclass(name = "World")]
struct PyWorld {
    world: Arc<RwLock<World<UserEvent>>>,
}

#[pymethods]
impl PyWorld {
    /// Adds an entity. Inactive ones are not yielded by [`PyWorld::entities`].
    #[pyo3(signature = (active = true))]
    fn spawn(&self, active: bool) -> Id {
        self.world.read().unwrap().spawn(active)
    }

    /// Removes an entity and every component attached to it.
    fn despawn(&self, entity: Id) {
        self.world.read().unwrap().despawn(entity);
    }

    /// The ids of every active entity.
    fn entities(&self) -> Vec<Id> {
        self.world.read().unwrap().entities()
    }

    /// How many active entities there are.
    fn entity_count(&self) -> usize {
        self.world.read().unwrap().entities().len()
    }

    /// The terminal the app is drawing with, or `None` when there is none yet.
    ///
    /// This is what a render function reaches the running terminal through: the
    /// frame is drawn from the terminal, so a function registered in
    /// `render_pipeline` always finds one. The file itself is read before the
    /// app builds the terminal, so calling this while the module runs finds
    /// nothing — which is why the access belongs in a system rather than at the
    /// top of the file.
    ///
    /// It is looked up wherever it is attached rather than by a known entity id,
    /// so a config that moves the app's components about still finds it.
    fn terminal(&self) -> Option<PyTerminal> {
        let world = self.world.read().unwrap();

        world
            .entities()
            .into_iter()
            .find_map(|entity| world.component::<Terminal>(entity))
            .map(|terminal| PyTerminal { terminal })
    }

    /// How many systems are registered, across every pipeline.
    fn system_count(&self) -> usize {
        self.world.read().unwrap().sm.read().unwrap().system_count()
    }

    /// Registers a Python function as a system of `pipeline`.
    ///
    /// The function is handed the world, once per event the app dispatches, on
    /// the thread the event loop runs on. It may read and write the world —
    /// including adding further systems.
    #[pyo3(signature = (func, pipeline = 0))]
    fn add_system(&self, py: Python<'_>, func: Py<PyAny>, pipeline: Id) -> PyResult<()> {
        if !func.bind(py).is_callable() {
            return Err(pyo3::exceptions::PyTypeError::new_err(
                "a system has to be callable",
            ));
        }

        self.world
            .read()
            .unwrap()
            .add_system(pipeline, PySystem { func });

        Ok(())
    }

    /// Removes the most recently added system of `pipeline`.
    #[pyo3(signature = (pipeline = 0))]
    fn remove_system(&self, pipeline: Id) {
        self.world.read().unwrap().sm.write().unwrap().rm(pipeline);
    }

    /// The base colour of the 3D lighting pass, as `(r, g, b)`.
    #[getter]
    fn ambient_color(&self) -> (f32, f32, f32) {
        let color = self.world.read().unwrap().ambient_color;

        (color.x, color.y, color.z)
    }

    #[setter]
    fn set_ambient_color(&self, color: (f32, f32, f32)) {
        self.world.write().unwrap().ambient_color = Vector3::new(color.0, color.1, color.2);
    }

    /// The intensity the ambient colour is scaled by.
    #[getter]
    fn ambient_intensity(&self) -> f32 {
        self.world.read().unwrap().ambient_intensity
    }

    #[setter]
    fn set_ambient_intensity(&self, intensity: f32) {
        self.world.write().unwrap().ambient_intensity = intensity;
    }
}

/// The running terminal, as the config file sees it.
///
/// A render function is handed the world once per frame, and this is how it
/// reaches what that frame is drawn from: where the cursor is, how big the grid
/// is, and the two colours the frame composites with. The window colour and the
/// cursor colour can be written, which is how the look is animated from Python;
/// the cursor's own state belongs to the shell, so it is read-only.
///
/// A change made here lands on the next frame, because a frame's uniform is
/// written before the render functions run. One frame is not a difference an eye
/// can see.
#[pyclass(name = "Terminal")]
struct PyTerminal {
    terminal: Arc<RwLock<Terminal>>,
}

/// A config's `(r, g, b)` as the `r, g, b, a` a terminal holds.
fn rgba(color: (f32, f32, f32)) -> [f32; 4] {
    [color.0, color.1, color.2, 1.0]
}

/// A terminal colour as the `(r, g, b)` the config file writes.
fn rgb(color: [f32; 4]) -> (f32, f32, f32) {
    (color[0], color[1], color[2])
}

#[pymethods]
impl PyTerminal {
    /// Where the cursor is, as `(column, row)`.
    #[getter]
    fn cursor(&self) -> (usize, usize) {
        self.terminal.read().unwrap().cursor_position
    }

    /// The grid size, as `(columns, rows)`.
    #[getter]
    fn size(&self) -> (usize, usize) {
        self.terminal.read().unwrap().size
    }

    /// Whether the cursor is being drawn at all: the shell hides it with
    /// `DECTCEM`, and a blinking one is hidden on alternate half-cycles.
    #[getter]
    fn cursor_visible(&self) -> bool {
        self.terminal.read().unwrap().cursor_visible
    }

    /// The cursor's shape, in the shader's numbers: 0 block, 1 bar, 2
    /// underline. Read-only, because it is the shell's to ask for with
    /// `DECSCUSR`.
    #[getter]
    fn cursor_style(&self) -> u32 {
        self.terminal.read().unwrap().cursor_style
    }

    /// One cell, as `(width, height)` in physical pixels.
    #[getter]
    fn cursor_size(&self) -> (f32, f32) {
        self.terminal.read().unwrap().cursor_size
    }

    /// The colour behind the grid, as `(r, g, b)`.
    #[getter]
    fn background(&self) -> (f32, f32, f32) {
        rgb(self.terminal.read().unwrap().background)
    }

    #[setter]
    fn set_background(&self, color: (f32, f32, f32)) {
        self.terminal.write().unwrap().set_background(rgba(color));
    }

    /// The colour the cursor is drawn in, as `(r, g, b)`.
    #[getter]
    fn cursor_color(&self) -> (f32, f32, f32) {
        rgb(self.terminal.read().unwrap().cursor_color)
    }

    #[setter]
    fn set_cursor_color(&self, color: (f32, f32, f32)) {
        self.terminal.write().unwrap().set_cursor_color(rgba(color));
    }
}

/// A Python callable, as a [`System`].
///
/// It is handed a [`PyWorld`] over the same handle, so a system written in the
/// config file can spawn entities, read or write the ambient values, and — from
/// a render function — reach the terminal the frame is drawn from. The event is
/// not passed on: it is the application's own, and its type is not something
/// Python can be told about.
struct PySystem {
    func: Py<PyAny>,
}

impl System<UserEvent> for PySystem {
    fn update(
        &mut self,
        _control: Arc<RwLock<Control<UserEvent>>>,
        world: Arc<RwLock<World<UserEvent>>>,
    ) -> anyhow::Result<()> {
        Python::attach(|py| -> PyResult<()> {
            let world = Py::new(py, PyWorld { world })?;

            self.func.bind(py).call1((world,))?;

            Ok(())
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Reload, counts};
    use crate::app::UserEvent;
    use crate::terminal::Terminal;
    use crate::terminal::font::Font;
    use crate::world::{Control, EVENT_PIPELINE, RENDER_PIPELINE, World};

    use nalgebra::Vector3;
    use winit::event::Event;

    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    /// A private directory to write test configs into.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hext-config-{}-{name}", std::process::id()));

        let _ = std::fs::remove_dir_all(&dir);

        dir.join("config.py")
    }

    fn world() -> Arc<RwLock<World<UserEvent>>> {
        World::new(Vector3::zeros(), 0.0)
    }

    /// One config, evaluated and then driven the way the event loop drives it.
    fn evaluate(name: &str, source: &str) -> (Arc<RwLock<World<UserEvent>>>, bool) {
        let path = scratch(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();

        let world = world();
        let loaded = Config::read_from(&path, Arc::clone(&world)).is_ok();

        // Driven the way the event loop does it, snapshot and all.
        let systems = world.read().unwrap().systems();
        systems.init(Arc::clone(&world)).unwrap();
        systems
            .update(Control::new(Event::AboutToWait), Arc::clone(&world))
            .unwrap();

        (world, loaded)
    }

    #[test]
    fn a_python_system_runs_once_per_event_and_is_handed_the_world() {
        let (world, loaded) = evaluate(
            "tick",
            "def tick(world):\n    world.ambient_intensity += 1.0\n    world.spawn()\n\nworld.add_system(tick)\n",
        );

        assert!(loaded);

        let world = world.read().unwrap();

        assert_eq!(world.sm.read().unwrap().system_count(), 1);
        assert_eq!(world.ambient_intensity, 1.0);
        assert_eq!(world.entities().len(), 1);
    }

    #[test]
    fn a_python_system_can_reach_back_into_the_worlds_manager() {
        // Reading the manager is what a system must be able to do without
        // deadlocking on a lock the runner is holding.
        let (world, loaded) = evaluate(
            "manager",
            "def tick(world):\n    world.ambient_intensity = float(world.system_count())\n\nworld.add_system(tick)\n",
        );

        assert!(loaded);
        assert_eq!(world.read().unwrap().ambient_intensity, 1.0);
    }

    #[test]
    fn a_system_that_is_not_callable_is_rejected() {
        let (world, loaded) = evaluate("not-callable", "world.add_system(42)\n");

        assert!(!loaded);

        // The failed load leaves the world's systems alone.
        assert_eq!(world.read().unwrap().sm.read().unwrap().system_count(), 0);
    }

    #[test]
    fn a_config_can_register_a_render_function() {
        // A system in the render pipeline is a render function: it runs when a
        // frame is drawn, and not with the events the app dispatches.
        let path = scratch("render");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "def draw(world):\n    world.ambient_intensity += 1.0\n\nworld.add_system(draw, pipeline=render_pipeline)\n",
        )
        .unwrap();

        let world = world();
        assert!(Config::read_from(&path, Arc::clone(&world)).is_ok());

        let systems = world.read().unwrap().systems();
        systems
            .update_pipeline(
                EVENT_PIPELINE,
                Control::new(Event::AboutToWait),
                Arc::clone(&world),
            )
            .unwrap();

        assert_eq!(world.read().unwrap().ambient_intensity, 0.0);

        systems
            .update_pipeline(
                RENDER_PIPELINE,
                Control::new(Event::AboutToWait),
                Arc::clone(&world),
            )
            .unwrap();

        assert_eq!(world.read().unwrap().ambient_intensity, 1.0);
    }

    /// A config file holding `source`, loaded without touching the systems.
    fn config(name: &str, source: &str) -> Config {
        let path = scratch(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();

        Config::read_from(&path, world()).unwrap()
    }

    #[test]
    fn the_shader_setting_is_the_source_the_config_wrote() {
        let shader = config("shader", "shader = '@vertex fn vs_screen() {}'\n");

        assert_eq!(shader.shader.as_deref(), Some("@vertex fn vs_screen() {}"));
    }

    #[test]
    fn a_config_without_a_shader_keeps_the_built_in_one() {
        assert_eq!(config("no-shader", "font_size = 30.0\n").shader, None);
        assert_eq!(config("none-shader", "shader = None\n").shader, None);
    }

    #[test]
    fn a_shader_setting_that_is_not_text_keeps_the_default() {
        assert_eq!(config("bad-shader", "shader = 42\n").shader, None);
    }

    #[test]
    fn the_background_image_setting_is_the_path_the_config_wrote() {
        let picture = config("background", "background_image = '/tmp/wall.png'\n");

        assert_eq!(
            picture.background_image.as_deref(),
            Some(std::path::Path::new("/tmp/wall.png"))
        );
    }

    #[test]
    fn a_config_without_a_background_image_has_none() {
        assert_eq!(
            config("no-background", "font_size = 30.0\n").background_image,
            None
        );
        assert_eq!(
            config("none-background", "background_image = None\n").background_image,
            None
        );
        assert_eq!(
            config("empty-background", "background_image = ''\n").background_image,
            None,
            "an empty name is no picture at all"
        );
        assert_eq!(
            config("bad-background", "background_image = 42\n").background_image,
            None
        );
    }

    #[test]
    fn the_foreground_image_setting_is_the_path_the_config_wrote() {
        let overlay = config("foreground", "foreground_image = '/tmp/mask.png'\n");

        assert_eq!(
            overlay.foreground_image.as_deref(),
            Some(std::path::Path::new("/tmp/mask.png"))
        );
    }

    #[test]
    fn a_config_without_a_foreground_image_has_none() {
        assert_eq!(
            config("no-foreground", "font_size = 30.0\n").foreground_image,
            None
        );
        assert_eq!(
            config("none-foreground", "foreground_image = None\n").foreground_image,
            None
        );
        assert_eq!(
            config("empty-foreground", "foreground_image = ''\n").foreground_image,
            None,
            "an empty name is no picture at all"
        );
    }

    #[test]
    fn the_animate_setting_is_the_flag_the_config_wrote() {
        assert!(config("animate-on", "animate = True\n").animate);
        assert!(!config("animate-off", "animate = False\n").animate);
        assert!(
            !config("animate-absent", "font_size = 30.0\n").animate,
            "a config that says nothing leaves the terminal still"
        );
        assert!(
            !config("animate-none", "animate = None\n").animate,
            "`None` is no answer, not a flag"
        );
        assert!(
            !config("animate-number", "animate = 1\n").animate,
            "the flag has to be a Python bool"
        );
    }

    /// The examples the repository ships, each evaluated as a config and driven
    /// a little, so one that no longer works is caught here rather than by a user.
    #[test]
    fn the_example_configs_are_configs_that_work() {
        let examples = [
            ("aurora", include_str!("../docs/config-aurora.py")),
            ("neon", include_str!("../docs/config-neon.py")),
        ];

        for (name, source) in examples {
            let path = scratch(&format!("example-{name}"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, source).unwrap();

            let world = world();
            let config =
                Config::read_from(&path, Arc::clone(&world)).expect("the example config");

            assert!(config.animate, "{name} asks for a frame every frame");

            let shader = config
                .shader
                .unwrap_or_else(|| panic!("{name} names a shader"));

            // The same front end wgpu compiles it with, so the shaders the
            // repository ships are known to build.
            let module = naga::front::wgsl::parse_str(&shader)
                .unwrap_or_else(|error| panic!("{name}'s shader must parse: {error}"));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::empty(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{name}'s shader must validate: {error}"));

            // Whatever it registered runs, the way a frame runs it — and the
            // aurora one finds no terminal to drift, which is what the app's own
            // order does too: the file is read before the terminal is built.
            let systems = world.read().unwrap().systems();
            systems
                .update_pipeline(
                    RENDER_PIPELINE,
                    Control::new(Event::AboutToWait),
                    Arc::clone(&world),
                )
                .unwrap();
        }
    }

    #[test]
    fn a_config_system_can_read_and_recolour_the_terminal() {
        let world = world();
        let font = Font::load(16.0).expect("a system monospace font");

        let terminal = {
            let world = world.read().unwrap();
            let entity = world.spawn(true);

            world.attach_value(
                entity,
                Terminal::new(Arc::new(RwLock::new(font)), [0.0; 4], [1.0; 4]),
            )
        };

        let path = scratch("terminal");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "def draw(world):\n    terminal = world.terminal()\n    if terminal is None:\n        return\n    assert terminal.cursor == (0, 0)\n    assert terminal.cursor_visible\n    assert terminal.cursor_style == 0\n    assert terminal.cursor_size[0] > 0.0\n    assert terminal.size == (0, 0)\n    terminal.cursor_color = (0.1, 0.2, 0.3)\n    terminal.background = (0.4, 0.5, 0.6)\n\nworld.add_system(draw, pipeline=render_pipeline)\n",
        )
        .unwrap();

        assert!(Config::read_from(&path, Arc::clone(&world)).is_ok());

        // Driven the way the renderer drives a render function.
        let systems = world.read().unwrap().systems();
        systems
            .update_pipeline(
                RENDER_PIPELINE,
                Control::new(Event::AboutToWait),
                Arc::clone(&world),
            )
            .unwrap();

        let terminal = terminal.read().unwrap();

        assert_eq!(terminal.cursor_color, [0.1, 0.2, 0.3, 1.0]);
        assert_eq!(terminal.background, [0.4, 0.5, 0.6, 1.0]);
        assert_eq!(terminal.size, (0, 0), "the grid, read back");
    }

    #[test]
    fn a_changed_config_is_reloaded_and_replaces_what_the_last_one_added() {
        let path = scratch("reload");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "def tick(world):\n    world.ambient_intensity += 1.0\n\nworld.add_system(tick)\n",
        )
        .unwrap();

        let world = world();
        let before = counts(&world);
        let config = Config::read_from(&path, Arc::clone(&world)).expect("the first file");
        let mut reload = Reload::new(Some(path.clone()), &before, &world);

        assert!(!config.animate);

        let count = || world.read().unwrap().sm.read().unwrap().system_count();
        assert_eq!(count(), 1, "the file's system");

        // Nothing has changed: the first poll has nothing to report.
        std::thread::sleep(Duration::from_millis(250));
        assert!(reload.reload(Arc::clone(&world)).is_none());

        // A save is picked up, and the systems the first file added are taken
        // back first, so what is left is what the new file says.
        std::fs::write(
            &path,
            "world.add_system(lambda world: None)\nanimate = True\n",
        )
        .unwrap();

        std::thread::sleep(Duration::from_millis(250));

        let config = reload.reload(Arc::clone(&world)).expect("the saved file");

        assert!(config.animate, "the flag the new file wrote");
        assert_eq!(count(), 1, "the new file's one system, not two");

        // And it really is the new file's system: the old one counted events.
        let systems = world.read().unwrap().systems();
        systems
            .update(Control::new(Event::AboutToWait), Arc::clone(&world))
            .unwrap();

        assert_eq!(world.read().unwrap().ambient_intensity, 0.0);
    }

    #[test]
    fn a_reload_of_a_broken_file_keeps_the_settings_and_drops_only_its_systems() {
        let path = scratch("reload-broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "world.add_system(lambda world: None)\nanimate = True\n").unwrap();

        let world = world();
        let before = counts(&world);
        assert!(Config::read_from(&path, Arc::clone(&world)).is_ok());

        let mut reload = Reload::new(Some(path.clone()), &before, &world);

        std::fs::write(&path, "this is not Python\n").unwrap();
        std::thread::sleep(Duration::from_millis(250));

        // The file cannot be evaluated, so there is nothing to apply — the
        // caller keeps what the terminal is already doing.
        assert!(reload.reload(Arc::clone(&world)).is_none());

        // The system that file added is gone, because a reload takes back what
        // the last evaluation put there before it runs the file again.
        assert_eq!(world.read().unwrap().sm.read().unwrap().system_count(), 0);
    }
}
