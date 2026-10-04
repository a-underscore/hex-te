mod app;
mod gpu;
mod pty;
mod terminal;

use winit::event_loop::EventLoop;

use crate::app::App;

pub(crate) const WINDOW_TITLE: &str = "hex-te";

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new()?;

    event_loop.run_app(&mut app)?;

    Ok(())
}
