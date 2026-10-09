//! Everything that draws: the window, the surface and the GPU, and the screen
//! draw a frame ends with.
//!
//! This is the only part of the crate that knows about wgpu. What it draws is
//! the terminal's grid, which [`crate::terminal`] turns into a texture of its
//! own, plus the cursor and any shader the config file brought with it.

pub(crate) mod drawable;
pub(crate) mod gpu;

pub(crate) use drawable::{Drawable, Pictures};
pub(crate) use gpu::Gpu;
