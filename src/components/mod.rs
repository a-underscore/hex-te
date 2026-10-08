//! Components attached to world entities: cameras, transforms and tags.
//!
//! `hex` also had `Light3` and `Model` in this module. Both own Vulkano shadow
//! maps or graphics pipelines, so they only move over once the wgpu renderer
//! does; see the crate docs.

pub use camera3::Camera3;
pub use tag::Tag;
pub use trans3::Trans3;

use nalgebra::Matrix4;

pub mod camera3;
pub mod tag;
pub mod trans3;

/// Converts an OpenGL-style clip-space projection to wgpu clip space.
///
/// `nalgebra`'s `Perspective3` produces OpenGL conventions: Y up and a depth
/// range of `-1..1`. wgpu's normalised device coordinates keep Y pointing up
/// too, but expect depth in `0..1`, so only the depth axis is remapped.
///
/// `hex`'s equivalent was `vulkan_clip_correction`, which *also* negated Y
/// because Vulkan's Y axis points down.
pub fn wgpu_clip_correction() -> Matrix4<f32> {
    // Column-major, as `Matrix4::new` expects: scale Z by 0.5, then shift it
    // by 0.5.
    Matrix4::new(
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0, 0.0, 1.0,
    )
}
