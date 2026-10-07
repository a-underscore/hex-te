use anyhow::Error;
use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};

pub(crate) struct Pty {
    pub pair: PtyPair,
    pub child: Box<dyn portable_pty::Child + Send>,
}

impl Pty {
    pub fn new() -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let cmd = CommandBuilder::new(env!("SHELL"));
        let child = pair.slave.spawn_command(cmd)?;

        writeln!(pair.master.take_writer()?, "ls -l\r\n")?;

        Ok(Self { pair, child })
    }

    pub fn read(&mut self, buffer: &mut Vec<u8>) -> Result<(), Error> {
        self.pair.master.try_clone_reader()?.read(buffer)?;

        Ok(())
    }

    pub fn write(&mut self, buffer: &mut [u8]) -> Result<(), Error> {
        self.pair.master.take_writer()?.write(buffer)?;

        Ok(())
    }
}
