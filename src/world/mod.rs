//! The entity-component world: an [`EntityManager`] plus global lighting
//! values that apply to every rendered frame.

pub mod entity_manager;

pub use entity_manager::EntityManager;

use std::sync::{Arc, RwLock};

use nalgebra::Vector3;

/// Shared world state. The [`EntityManager`] holds every entity and its
/// components; the ambient fields feed the 3D lighting pass so scenes are
/// never fully black outside direct light.
pub struct World {
    pub em: Arc<RwLock<EntityManager>>,
    pub ambient_color: Vector3<f32>,
    pub ambient_intensity: f32,
}

impl World {
    pub fn new(
        em: Arc<RwLock<EntityManager>>,
        ambient_color: Vector3<f32>,
        ambient_intensity: f32,
    ) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            em,
            ambient_color,
            ambient_intensity,
        }))
    }

    /// The ambient values in the layout the lighting shader declares.
    ///
    /// In `hex` these two fields were copied into a Vulkano descriptor subbuffer
    /// every frame; wgpu instead wants a `bytemuck::Pod` value it can hand to
    /// `Queue::write_buffer`, which is what this returns.
    pub fn ambient_uniform(&self) -> AmbientUniform {
        AmbientUniform {
            color: self.ambient_color.into(),
            intensity: self.ambient_intensity,
        }
    }
}

/// `vec3 ambient_color; float ambient_intensity;` — WGSL pads a `vec3` to 16
/// bytes, and `[f32; 3]` followed by `f32` reproduces that layout exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct AmbientUniform {
    pub color: [f32; 3],
    pub intensity: f32,
}

#[cfg(test)]
mod tests {
    use super::{AmbientUniform, World};
    use crate::world::EntityManager;
    use nalgebra::Vector3;

    #[test]
    fn ambient_values_reach_the_shader_layout() {
        let world = World::new(EntityManager::new(), Vector3::new(0.1, 0.2, 0.3), 2.0);
        let uniform = world.read().unwrap().ambient_uniform();

        assert_eq!(uniform.color, [0.1, 0.2, 0.3]);
        assert_eq!(uniform.intensity, 2.0);
    }

    #[test]
    fn the_ambient_uniform_is_padding_free() {
        // A wgpu uniform buffer is written from a `bytemuck::Pod` value, which
        // requires the struct to have no padding: 3 floats plus an intensity.
        assert_eq!(size_of::<AmbientUniform>(), 16);
    }
}
