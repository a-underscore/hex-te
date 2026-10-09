//! The configuration file: a Python module in the user's config directory,
//! evaluated with an embedded interpreter.
//!
//! On first run the app writes a documented `config.py` there and reads it back;
//! every setting is optional and falls back to the default the file documents.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use nalgebra::Vector3;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::WINDOW_TITLE;
use crate::app::UserEvent;
use crate::control::Control;
use crate::id::Id;
use crate::world::{RENDER_PIPELINE, System, World};

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
    /// The WGSL the screen pipeline is built from, as source. `None` keeps the
    /// shader compiled into the binary, which is what the app ships with.
    pub shader: Option<String>,
    /// A picture to draw behind the grid. `None` draws the plain background
    /// colour, which is what the shader does when no picture is there at all.
    pub background_image: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            window_size: (1024, 640),
            shell: None,
            background: [0.05, 0.06, 0.08, 1.0],
            cursor_color: [0.16, 0.72, 0.72, 1.0],
            shader: None,
            background_image: None,
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
# `shader` and `background_image` are the settings the app reads back today; the
# others are written out for the day they are wired up, so changing them has no
# effect yet.

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
# bindings `src/shaders/screen.wgsl` uses, which is also what `None` falls back
# to. Because this is Python, a file can be read instead of pasted:
#
#     shader = open("/home/you/.config/hext/crt.wgsl").read()
shader = None

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
# loop has nothing left to do. What it can reach today is the world itself, the
# entities and the ambient values: the components the terminal draws with are
# not exposed to Python yet.
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

/// A Python callable, as a [`System`].
///
/// It is handed a [`PyWorld`] over the same handle, so a system written in the
/// config file can spawn entities and read or write the ambient values. The
/// event is not passed on: it is the application's own, and its type is not
/// something Python can be told about.
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
    use super::Config;
    use crate::app::UserEvent;
    use crate::control::Control;
    use crate::world::{EVENT_PIPELINE, RENDER_PIPELINE, World};

    use nalgebra::Vector3;
    use winit::event::Event;

    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

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
}
