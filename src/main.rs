mod app;
mod gpu;
mod pty;
mod terminal;

use winit::event_loop::EventLoop;

use crate::app::{App, UserEvent};

pub(crate) const WINDOW_TITLE: &str = "hex-te";

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy)?;

    event_loop.run_app(&mut app)?;

    Ok(())
}
