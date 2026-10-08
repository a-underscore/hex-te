//! The configuration file: a Python module in the user's config directory,
//! evaluated with an embedded interpreter.
//!
//! On first run the app writes a documented `config.py` there and reads it back;
//! every setting is optional and falls back to the default the file documents.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::WINDOW_TITLE;

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
const DEFAULT_CONFIG: &str = r#"# hex-te configuration.
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
"#;

impl Config {
    /// Where the config lives: `$XDG_CONFIG_HOME/hex-te/config.py`, or
    /// `$HOME/.config/hex-te/config.py` when `XDG_CONFIG_HOME` is unset.
    pub fn path() -> anyhow::Result<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => PathBuf::from(
                std::env::var_os("HOME")
                    .ok_or_else(|| anyhow::anyhow!("neither XDG_CONFIG_HOME nor HOME is set"))?,
            )
            .join(".config"),
        };

        Ok(base.join("hex-te").join("config.py"))
    }

    /// Reads the config, creating it from [`DEFAULT_CONFIG`] if it is not there
    /// yet.
    ///
    /// Never fails: a config that cannot be found or evaluated is reported and
    /// the defaults are used, so a broken file cannot stop the terminal starting.
    pub fn load() -> Self {
        let path = match Self::path() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error}");
                return Self::default();
            }
        };

        match Self::read_from(&path) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("{WINDOW_TITLE}: {error:#}");
                Self::default()
            }
        }
    }

    /// Creates `path` if it does not exist, then evaluates it.
    pub fn read_from(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            std::fs::write(path, DEFAULT_CONFIG)?;
        }

        Self::evaluate(&std::fs::read_to_string(path)?)
    }

    /// Runs `source` as a Python module and reads the settings out of its
    /// globals.
    fn evaluate(source: &str) -> anyhow::Result<Self> {
        let source = CString::new(source)?;
        let mut config = Self::default();

        Python::attach(|py| -> PyResult<()> {
            let globals = PyDict::new(py);
            py.run(source.as_c_str(), Some(&globals), None)?;

            if let Some(size) = number(&globals, "font_size") {
                config.font_size = size.max(1.0);
            }

            if let Some(width) = integer(&globals, "window_width") {
                config.window_size.0 = width.max(1);
            }

            if let Some(height) = integer(&globals, "window_height") {
                config.window_size.1 = height.max(1);
            }

            if let Some(shell) = text(&globals, "shell") {
                config.shell = shell;
            }

            if let Some(background) = color(&globals, "background") {
                config.background = background;
            }

            if let Some(cursor) = color(&globals, "cursor_color") {
                config.cursor_color = cursor;
            }

            Ok(())
        })?;

        Ok(config)
    }
}

/// One name from the module's globals, or `None` when the file does not set it.
fn setting<'py>(globals: &Bound<'py, PyDict>, name: &str) -> Option<Bound<'py, PyAny>> {
    match globals.get_item(name) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: config.py: cannot look up {name}: {error}");
            None
        }
    }
}

fn number(globals: &Bound<'_, PyDict>, name: &str) -> Option<f32> {
    let value = setting(globals, name)?;

    match value.extract::<f32>() {
        Ok(number) => Some(number),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: config.py: {name} is not a number ({error})");
            None
        }
    }
}

fn integer(globals: &Bound<'_, PyDict>, name: &str) -> Option<u32> {
    let value = setting(globals, name)?;

    match value.extract::<u32>() {
        Ok(number) => Some(number),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: config.py: {name} is not a whole number ({error})");
            None
        }
    }
}

/// Returns `Some(None)` for an explicit `None`, which means "no override".
fn text(globals: &Bound<'_, PyDict>, name: &str) -> Option<Option<String>> {
    let value = setting(globals, name)?;

    match value.extract::<Option<String>>() {
        Ok(text) => Some(text),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: config.py: {name} is not text ({error})");
            None
        }
    }
}

/// A colour is written as a three-tuple; the alpha channel stays opaque.
fn color(globals: &Bound<'_, PyDict>, name: &str) -> Option<[f32; 4]> {
    let value = setting(globals, name)?;

    match value.extract::<(f32, f32, f32)>() {
        Ok((r, g, b)) => Some([r, g, b, 1.0]),
        Err(error) => {
            eprintln!("{WINDOW_TITLE}: config.py: {name} is not an (r, g, b) tuple ({error})");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, DEFAULT_CONFIG};

    use std::path::PathBuf;

    /// A private directory to write test configs into.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hex-te-config-{}-{name}", std::process::id()));

        let _ = std::fs::remove_dir_all(&dir);

        dir.join("config.py")
    }

    #[test]
    fn a_missing_file_is_created_with_the_documented_defaults() {
        let path = scratch("created");
        let config = Config::read_from(&path).expect("config");

        let written = std::fs::read_to_string(&path).expect("the file was created");

        assert_eq!(written, DEFAULT_CONFIG);
        assert_eq!(config.font_size, 16.0);
        assert_eq!(config.window_size, (1024, 640));
        assert_eq!(config.shell, None);
        assert_eq!(config.background, [0.05, 0.06, 0.08, 1.0]);
    }

    #[test]
    fn the_file_overrides_the_defaults() {
        let path = scratch("overrides");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "font_size = 12.5\nwindow_width = 800\nwindow_height = 600\nshell = \"/bin/sh\"\nbackground = (1.0, 0.0, 0.0)\n",
        )
        .unwrap();

        let config = Config::read_from(&path).expect("config");

        assert_eq!(config.font_size, 12.5);
        assert_eq!(config.window_size, (800, 600));
        assert_eq!(config.shell.as_deref(), Some("/bin/sh"));
        assert_eq!(config.background, [1.0, 0.0, 0.0, 1.0]);
        // Not mentioned by the file, so still the default.
        assert_eq!(config.cursor_color, [0.16, 0.72, 0.72, 1.0]);
    }

    #[test]
    fn python_expressions_and_broken_settings_are_handled() {
        let path = scratch("expressions");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "import math\nfont_size = math.floor(15.7)\nwindow_width = 'wide'\n",
        )
        .unwrap();

        let config = Config::read_from(&path).expect("config");

        // The expression is evaluated ...
        assert_eq!(config.font_size, 15.0);
        // ... and the setting whose type is wrong keeps its default.
        assert_eq!(config.window_size.0, 1024);
    }

    #[test]
    fn a_broken_file_is_an_error_the_caller_can_fall_back_from() {
        let path = scratch("broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "this is not python\n").unwrap();

        assert!(Config::read_from(&path).is_err());
    }
}
