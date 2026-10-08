use std::io::{Read, Write};
use std::sync::Mutex;
use std::thread;

use anyhow::Error;
use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};

/// The size the pty starts at. `terminal.rs` builds its first grid at the same
/// size so output written before the first frame is not clipped.
pub(crate) const INITIAL_COLS: u16 = 80;
pub(crate) const INITIAL_ROWS: u16 = 24;

/// How many bytes the reader thread asks for at a time.
const READ_CHUNK: usize = 8192;

/// A shell running on its own pty.
///
/// `portable_pty`'s master, writer and child are only `Send`, so each one sits
/// behind a mutex: that is what makes a `Pty` a legal ECS component, whose bound
/// is `Send + Sync`. Every method therefore takes `&self`.
pub(crate) struct Pty {
    pair: Mutex<PtyPair>,
    child: Mutex<Box<dyn portable_pty::Child + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    size: Mutex<(u16, u16)>,
}

impl Pty {
    /// Spawns `$SHELL` on a freshly allocated pty.
    ///
    /// `on_output` is called from a dedicated thread with each chunk the shell
    /// writes, and with `None` once it has exited. Reads from a pty block, so
    /// they must never happen on the event-loop thread.
    pub fn new(mut on_output: impl FnMut(Option<&[u8]>) + Send + 'static) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let child = pair.slave.spawn_command(CommandBuilder::new(shell))?;

        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        thread::Builder::new()
            .name("pty-reader".to_string())
            .spawn(move || {
                let mut chunk = [0u8; READ_CHUNK];

                loop {
                    match reader.read(&mut chunk) {
                        // A closed pty reports EOF or EIO depending on the
                        // platform; either one means the shell is gone.
                        Ok(0) | Err(_) => break,
                        Ok(read) => on_output(Some(&chunk[..read])),
                    }
                }

                on_output(None);
            })?;

        Ok(Self {
            pair: Mutex::new(pair),
            child: Mutex::new(child),
            writer: Mutex::new(writer),
            size: Mutex::new((INITIAL_COLS, INITIAL_ROWS)),
        })
    }

    /// Forwards bytes to the shell as if they had been typed.
    pub fn write(&self, bytes: &[u8]) -> Result<(), Error> {
        let mut writer = self.writer.lock().unwrap();

        writer.write_all(bytes)?;
        writer.flush()?;

        Ok(())
    }

    /// Tells the shell how big the grid is so full-screen programs lay out
    /// correctly: the pty turns this into a `SIGWINCH` for the child.
    pub fn resize(&self, cols: usize, rows: usize) -> Result<(), Error> {
        let cols = cols.min(u16::MAX as usize) as u16;
        let rows = rows.min(u16::MAX as usize) as u16;

        {
            let mut size = self.size.lock().unwrap();

            if (cols, rows) == *size || cols == 0 || rows == 0 {
                return Ok(());
            }

            *size = (cols, rows);
        }

        self.pair.lock().unwrap().master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        Ok(())
    }

    /// Kills the shell.
    pub fn kill(&self) -> Result<(), Error> {
        self.child.lock().unwrap().kill()?;

        Ok(())
    }
}
