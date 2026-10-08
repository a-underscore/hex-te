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
use crate::world::{System, World};

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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font_size: 16.0,
            window_size: (1024, 640),
            shell: None,
            background: [0.05, 0.06, 0.08, 1.0],
            cursor_color: [0.16, 0.72, 0.72, 1.0],
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

# The app's entity-component world is bound here as `world`. It is the same
# object the engine keeps using, so a config can seed or reconfigure it:
#
#   world.spawn(active=True) -> int    # add an entity
#   world.despawn(entity)              # remove it again
#   world.entities() -> [int]          # the ids of the active entities
#   world.entity_count() -> int
#   world.ambient_color = (r, g, b)    # base colour of the 3D lighting pass
#   world.ambient_intensity = 1.0
#
# It also holds the systems the app runs, so a config can add behaviour of its
# own. A Python function added as a system is called with the world once per
# event the app dispatches:
#
#   world.add_system(fn, pipeline=0)   # fn(world), every event
#   world.remove_system(pipeline=0)    # drops the most recent one
#   world.system_count() -> int
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
        let config = Self::default();

        Python::attach(|py| -> PyResult<()> {
            let globals = PyDict::new(py);

            globals.set_item("world", Py::new(py, PyWorld { world })?)?;

            py.run(source.as_c_str(), Some(&globals), None)?;

            Ok(())
        })?;

        Ok(config)
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
    use crate::world::World;

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
}
