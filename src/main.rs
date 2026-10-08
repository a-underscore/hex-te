//! A GPU-accelerated terminal emulator, plus the engine core being ported from
//! the Vulkano project in `../hex`.
//!
//! The application is [`app::App`]; the engine is [`world`] (an entity-component
//! store), [`components`] (the engine's own components) and [`control`] (the
//! event value a system is handed each turn). The app's own resources —
//! [`gpu::Gpu`], [`font::Font`], [`terminal::Terminal`] and [`pty::Pty`] — are
//! components of that same world, which is why they all live in one crate
//! instead of behind a library target.

mod app;
// The engine core is ported ahead of what the terminal itself uses: the 3D
// components, and the parts of the world API only the renderer will need, have
// no caller yet. These allow-lists go away as that lands.
#[allow(dead_code)]
mod components;
mod control;
mod font;
mod gpu;
mod id;
mod pty;
mod terminal;
#[allow(dead_code)]
mod world;

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
