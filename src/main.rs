//! A GPU-accelerated terminal emulator: a shell on a pty, a VT parser that turns
//! its output into a character grid, and a wgpu renderer that draws the grid.
//!
//! Everything the app needs lives in one entity-component [`world`]: [`gpu::Gpu`],
//! [`font::Font`], [`terminal::Terminal`] and [`pty::Pty`] are components of one
//! entity, and the behaviour is a system that borrows them back out of the world.
//! [`control`] is the event value a system is handed each turn. Keeping them in
//! one crate is what lets the resources and the behaviour share that world
//! without a library target in between.

mod app;
mod clipboard;
mod config;
mod control;
mod drawable;
mod font;
mod glyphs;
mod gpu;
mod id;
mod pty;
mod terminal;
// The world carries a little more than the terminal uses today — the component
// and system managers, and the ambient values a renderer will start from — so
// its allow-list stays until the renderer claims the rest.
#[allow(dead_code)]
mod world;

use winit::event_loop::EventLoop;

use crate::app::{App, UserEvent};

pub(crate) const WINDOW_TITLE: &str = "hext";

fn main() -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy)?;

    event_loop.run_app(&mut app)?;

    Ok(())
}
