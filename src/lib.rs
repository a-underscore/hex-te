//! The engine core of `hex`, ported from Vulkano to `wgpu`.
//!
//! `hex` is a small toy engine built around an entity-component
//! [`world::World`], a set of [`components`] and a pipeline of systems. This
//! library target is where that code is being translated.
//!
//! | `hex` | state here |
//! | --- | --- |
//! | `world/` — `World`, `EntityManager`, component storage | ported |
//! | `control.rs` — `Control`, the winit event plus an `exit` flag | ported |
//! | `world/system_manager/` (from the 0.3.0 line) | ported, run on the calling thread |
//! | `components/` — `Camera3`, `Trans3`, `Tag` | ported |
//! | `components/` — `Light3`, `Model`, `renderables/`, `renderers/` | still Vulkano; they follow once the renderer does |
//!
//! Two things are deliberately different from `hex` because the GPU API is: the
//! ambient lighting values are handed over as [`world::AmbientUniform`], a
//! padding-free `bytemuck::Pod` struct for `Queue::write_buffer`, and the
//! clip-space correction applied to camera projections is depth-only, because
//! wgpu's Y axis — unlike Vulkan's — already points up. See
//! [`components::wgpu_clip_correction`].
//!
//! The terminal emulator in `src/main.rs` is a separate target in the same
//! package and knows nothing about this one; it exists so the port can be built
//! and tested here.

pub mod components;
pub mod control;
pub mod id;
pub mod world;

pub use anyhow;
pub use control::Control;
pub use id::Id;
pub use nalgebra;
pub use winit;
pub use world::{System, SystemManager, World};
