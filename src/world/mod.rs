//! The entity-component world: an [`EntityManager`] plus global lighting
//! values that apply to every rendered frame.

pub mod entity_manager;
pub mod system_manager;

pub use entity_manager::EntityManager;
pub use system_manager::{System, SystemManager};

use std::sync::{Arc, RwLock};

use nalgebra::Vector3;

use crate::Id;
use crate::components::{Camera3, Tag, Trans3};

/// Shared world state. The [`EntityManager`] holds every entity and its
/// components; the ambient fields feed the 3D lighting pass so scenes are
/// never fully black outside direct light.
pub struct World {
    pub em: Arc<RwLock<EntityManager>>,
    pub ambient_color: Vector3<f32>,
    pub ambient_intensity: f32,
}

impl World {
    /// Builds a world with its own entity manager, with the engine's component
    /// managers already registered.
    pub fn new(ambient_color: Vector3<f32>, ambient_intensity: f32) -> Arc<RwLock<Self>> {
        Self::from_manager(EntityManager::new(), ambient_color, ambient_intensity)
    }

    /// Like [`World::new`], but over an entity manager you already have.
    ///
    /// This is `hex`'s `World::new` signature: the ambient values are the only
    /// part of a world that does not already live in the entity manager.
    pub fn from_manager(
        em: Arc<RwLock<EntityManager>>,
        ambient_color: Vector3<f32>,
        ambient_intensity: f32,
    ) -> Arc<RwLock<Self>> {
        {
            let mut em = em.write().unwrap();

            // Registering up front leaves the component map holding a manager
            // for every engine component type, rather than creating them as a
            // side effect of the first `attach`.
            em.register::<Camera3>();
            em.register::<Tag>();
            em.register::<Trans3>();
        }

        Arc::new(RwLock::new(Self {
            em,
            ambient_color,
            ambient_intensity,
        }))
    }

    /// Adds an entity. `active` decides whether [`EntityManager::entities`]
    /// yields it.
    pub fn spawn(&self, active: bool) -> Id {
        self.em.write().unwrap().add(active)
    }

    /// Removes an entity and every component attached to it.
    pub fn despawn(&self, eid: Id) {
        self.em.write().unwrap().rm(eid);
    }

    /// Attaches a component to an entity.
    ///
    /// Components are built as `Arc<RwLock<C>>` (see [`Trans3::new`]), so this
    /// is the only place that has to reach into the component managers:
    /// callers hold a world and nothing else.
    pub fn attach<C: Send + Sync + 'static>(&self, eid: Id, component: Arc<RwLock<C>>) {
        self.em.write().unwrap().add_component(eid, component);
    }

    /// Detaches an entity's component of type `C`, if it has one.
    pub fn detach<C: Send + Sync + 'static>(&self, eid: Id) {
        self.em.write().unwrap().rm_component::<C>(eid);
    }

    /// A shared handle to an entity's component of type `C`.
    ///
    /// The handle outlives the world's own lock, which is what lets two
    /// components be borrowed mutably at the same time:
    ///
    /// ```
    /// # use hex_te::components::Trans3;
    /// # use hex_te::nalgebra::Vector3;
    /// # use hex_te::World;
    /// let world = World::new(Vector3::zeros(), 0.0);
    /// let world = world.read().unwrap();
    ///
    /// let eid = world.spawn(true);
    /// world.attach(eid, Trans3::new(Vector3::zeros(), Vector3::zeros(), Vector3::new(1.0, 1.0, 1.0)));
    ///
    /// let transform = world.component::<Trans3>(eid).expect("transform");
    /// transform.write().unwrap().position = Vector3::new(0.0, 1.0, 0.0);
    /// ```
    pub fn component<C: Send + Sync + 'static>(&self, eid: Id) -> Option<Arc<RwLock<C>>> {
        self.em.read().unwrap().get_component::<C>(eid)
    }

    /// The ids of every active entity.
    pub fn entities(&self) -> Vec<Id> {
        self.em.read().unwrap().entities().collect()
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
    use crate::components::{Camera3, Tag, Trans3};
    use nalgebra::Vector3;

    use std::sync::{Arc, RwLock};

    fn world() -> Arc<RwLock<World>> {
        World::new(Vector3::zeros(), 0.0)
    }

    #[test]
    fn ambient_values_reach_the_shader_layout() {
        let world = World::new(Vector3::new(0.1, 0.2, 0.3), 2.0);
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

    #[test]
    fn the_engine_component_managers_are_registered_up_front() {
        let world = world();
        let em = world.read().unwrap();
        let em = em.em.read().unwrap();

        assert!(em.get_component_manager::<Camera3>().is_some());
        assert!(em.get_component_manager::<Tag>().is_some());
        assert!(em.get_component_manager::<Trans3>().is_some());
    }

    #[test]
    fn components_are_attached_and_fetched_through_the_world() {
        let world = world();
        let world = world.read().unwrap();

        let eid = world.spawn(true);
        let scale = Vector3::new(1.0, 2.0, 3.0);
        world.attach(eid, Trans3::new(Vector3::zeros(), Vector3::zeros(), scale));

        let transform = world.component::<Trans3>(eid).expect("transform");
        assert_eq!(transform.read().unwrap().scale, scale);
        assert_eq!(world.entities(), vec![eid]);

        world.detach::<Trans3>(eid);

        assert!(world.component::<Trans3>(eid).is_none());
    }

    #[test]
    fn despawning_drops_the_entity_and_its_components() {
        let world = world();
        let world = world.read().unwrap();

        let eid = world.spawn(true);
        world.attach(eid, Tag::new("player"));

        world.despawn(eid);

        assert!(world.entities().is_empty());
        assert!(world.component::<Tag>(eid).is_none());
    }
}
