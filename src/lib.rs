//! The engine core of `hex`, ported from Vulkano to `wgpu`.
//!
//! `hex` is a small toy engine built around an entity-component
//! [`world::World`]. This library target is where that code is being translated:
//! the world itself is renderer-agnostic, so the port is mostly about the
//! pieces that feed the GPU — see [`world::AmbientUniform`], the layout wgpu
//! needs for the ambient values the lighting pass reads.
//!
//! The terminal emulator in `src/main.rs` is a separate target in the same
//! package and knows nothing about this one; it exists so the port can be built
//! and tested here.

pub mod id;
pub mod world;

pub use id::Id;
pub use world::World;
