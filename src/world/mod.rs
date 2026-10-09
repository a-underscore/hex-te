//! The entity-component world: an [`EntityManager`] and a [`SystemManager`],
//! the [`Id`] handle and the [`Control`] value a system is handed, plus global
//! lighting values that apply to every rendered frame.

pub mod control;
pub mod entity_manager;
pub mod id;
pub mod system_manager;

pub use control::Control;
pub use entity_manager::EntityManager;
pub use id::Id;
pub use system_manager::{System, SystemManager};

use std::sync::{Arc, RwLock};

use nalgebra::Vector3;

/// The pipeline the application's own systems live in: the ones that handle the
/// events the event loop dispatches.
///
/// Pipelines are just numbers, and a program is free to use others of its own;
/// these two are the ones the app itself assumes something about.
pub const EVENT_PIPELINE: Id = 0;

/// The pipeline the renderer runs: a system added to it is called once per
/// frame, while the frame is being drawn, rather than once per event.
///
/// This is how a render function written in `config.py` is registered, which is
/// why the name is bound in the config file as `render_pipeline`.
pub const RENDER_PIPELINE: Id = 1;

/// Shared world state. The [`EntityManager`] holds every entity and its
/// components, the [`SystemManager`] the behaviour that runs over them, and the
/// ambient fields are the global lighting values a renderer starts from.
///
/// `E` is the application's winit user event — the same one [`Control`] carries
/// — so a system can be written against the events its app produces. The
/// default keeps the engine usable with no user event at all.
pub struct World<E: 'static = ()> {
    pub em: Arc<RwLock<EntityManager>>,
    pub sm: Arc<RwLock<SystemManager<E>>>,
    pub ambient_color: Vector3<f32>,
    pub ambient_intensity: f32,
}

impl<E: 'static> World<E> {
    /// Builds a world with an empty entity and system manager.
    pub fn new(ambient_color: Vector3<f32>, ambient_intensity: f32) -> Arc<RwLock<Self>> {
        Self::from_manager(
            EntityManager::new(),
            SystemManager::new(),
            ambient_color,
            ambient_intensity,
        )
    }

    /// Like [`World::new`], but over the managers you already have.
    ///
    /// The ambient values are the only part of a world that does not already
    /// live in a manager.
    pub fn from_manager(
        em: Arc<RwLock<EntityManager>>,
        sm: SystemManager<E>,
        ambient_color: Vector3<f32>,
        ambient_intensity: f32,
    ) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            em,
            sm: Arc::new(RwLock::new(sm)),
            ambient_color,
            ambient_intensity,
        }))
    }

    /// Adds a system to the world's own pipeline `pid`.
    ///
    /// Safe to call at any time, including from inside a running system: the
    /// systems run from a [`World::systems`] snapshot, not from under the
    /// manager's lock.
    pub fn add_system<S: System<E>>(&self, pid: Id, system: S) {
        self.sm.write().unwrap().add(pid, system);
    }

    /// A snapshot of the world's systems, ready to [`init`](SystemManager::init)
    /// or [`update`](SystemManager::update).
    ///
    /// The snapshot is taken out of the manager's lock on purpose: the caller
    /// runs the systems with the world and its manager free, so a system can
    /// reach back into either without deadlocking. A system added while they run
    /// joins the next snapshot, and so starts on the next event.
    pub fn systems(&self) -> SystemManager<E> {
        self.sm.read().unwrap().clone()
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
    /// The handle is what the rest of the code uses to reach the component, so
    /// this is the only place that has to reach into the component managers:
    /// callers hold a world and nothing else.
    pub fn attach<C: Send + Sync + 'static>(&self, eid: Id, component: Arc<RwLock<C>>) {
        self.em.write().unwrap().add_component(eid, component);
    }

    /// Attaches a component built as a plain value, handing back the handle the
    /// rest of the code uses to reach it.
    pub fn attach_value<C: Send + Sync + 'static>(&self, eid: Id, value: C) -> Arc<RwLock<C>> {
        let component = Arc::new(RwLock::new(value));

        self.attach(eid, Arc::clone(&component));

        component
    }

    /// Detaches an entity's component of type `C`, if it has one.
    pub fn detach<C: Send + Sync + 'static>(&self, eid: Id) {
        self.em.write().unwrap().rm_component::<C>(eid);
    }

    /// A shared handle to an entity's component of type `C`.
    ///
    /// The handle outlives the world's own lock, which is what lets two
    /// components be borrowed mutably at the same time. Given a component type
    /// `Position`, say:
    ///
    /// ```ignore
    /// let world = World::new(Vector3::zeros(), 0.0);
    /// let world = world.read().unwrap();
    ///
    /// let eid = world.spawn(true);
    /// world.attach_value(eid, Position([0.0, 0.0, 0.0]));
    ///
    /// let position = world.component::<Position>(eid).expect("position");
    /// position.write().unwrap().0 = [0.0, 1.0, 0.0];
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
    /// The result is `bytemuck::Pod`, so it can be written straight into a
    /// uniform buffer with `Queue::write_buffer`.
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
    use super::{AmbientUniform, System, World};
    use crate::world::Control;
    use nalgebra::Vector3;
    use winit::event::Event;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, RwLock};

    fn world() -> Arc<RwLock<World>> {
        World::new(Vector3::zeros(), 0.0)
    }

    /// A component type of the tests' own: the world stores any `Send + Sync`
    /// value, so nothing else has to exist for these to mean something.
    struct Marker(u32);

    /// Counts the frames it has been updated for.
    struct Frames(Arc<AtomicUsize>);

    impl System for Frames {
        fn update(
            &mut self,
            _control: Arc<RwLock<Control>>,
            _world: Arc<RwLock<World>>,
        ) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);

            Ok(())
        }
    }

    #[test]
    fn ambient_values_reach_the_shader_layout() {
        let world: Arc<RwLock<World>> = World::new(Vector3::new(0.1, 0.2, 0.3), 2.0);
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
    fn components_are_attached_and_fetched_through_the_world() {
        let world = world();
        let world = world.read().unwrap();

        let eid = world.spawn(true);
        let marker = world.attach_value(eid, Marker(1));

        assert_eq!(marker.read().unwrap().0, 1);
        assert_eq!(world.entities(), vec![eid]);
        assert!(world.component::<Marker>(eid).is_some());

        world.detach::<Marker>(eid);

        assert!(world.component::<Marker>(eid).is_none());
    }

    #[test]
    fn a_system_added_to_the_world_runs_through_the_worlds_manager() {
        let world = world();
        let frames = Arc::new(AtomicUsize::new(0));

        world
            .read()
            .unwrap()
            .add_system(0, Frames(Arc::clone(&frames)));

        // Driven the way the event loop does it: a snapshot of the systems,
        // taken so the world and its manager are free while they run.
        let systems = world.read().unwrap().systems();
        systems.init(Arc::clone(&world)).unwrap();
        systems
            .update(Control::new(Event::AboutToWait), Arc::clone(&world))
            .unwrap();

        assert_eq!(frames.load(Ordering::Relaxed), 1);
        assert_eq!(world.read().unwrap().sm.read().unwrap().system_count(), 1);
    }

    #[test]
    fn despawning_drops_the_entity_and_its_components() {
        let world = world();
        let world = world.read().unwrap();

        let eid = world.spawn(true);
        world.attach_value(eid, Marker(1));

        world.despawn(eid);

        assert!(world.entities().is_empty());
        assert!(world.component::<Marker>(eid).is_none());
    }
}
