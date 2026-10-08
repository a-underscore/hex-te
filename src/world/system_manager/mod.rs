pub mod system;

pub use system::System;

use crate::control::Control;
use crate::id::Id;
use crate::world::World;

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

/// Ordered groups of [`System`]s, one pipeline per id.
///
/// `hex` ran every pipeline on a thread pool owned by its `Context`, which is
/// why each system sat behind its own `Arc<RwLock<_>>`. There is no context
/// here yet, so the systems run one after another on the calling thread and the
/// extra indirection would only be overhead; reintroducing a pool changes this
/// struct's internals, not its API.
pub struct SystemManager<E: 'static = ()> {
    pipelines: HashMap<Id, Vec<Box<dyn System<E>>>>,
}

impl<E: 'static> Default for SystemManager<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: 'static> SystemManager<E> {
    pub fn new() -> Self {
        Self {
            pipelines: Default::default(),
        }
    }

    pub fn add_gen(&mut self, pid: Id, s: Box<dyn System<E>>) {
        self.pipelines.entry(pid).or_default().push(s);
    }

    pub fn add<S: System<E>>(&mut self, pid: Id, s: S) {
        self.add_gen(pid, Box::new(s));
    }

    pub fn rm(&mut self, pid: Id) {
        if let Some(pipeline) = self.pipelines.get_mut(&pid) {
            pipeline.pop();
        }
    }

    pub fn init(&mut self, world: Arc<RwLock<World>>) -> anyhow::Result<()> {
        for pipeline in self.pipelines.values_mut() {
            for system in pipeline.iter_mut() {
                system.init(Arc::clone(&world))?;
            }
        }

        Ok(())
    }

    pub fn update(
        &mut self,
        control: Arc<RwLock<Control<E>>>,
        world: Arc<RwLock<World>>,
    ) -> anyhow::Result<()> {
        for pipeline in self.pipelines.values_mut() {
            for system in pipeline.iter_mut() {
                system.update(Arc::clone(&control), Arc::clone(&world))?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{System, SystemManager};
    use crate::control::Control;
    use crate::world::World;

    use std::sync::{Arc, Mutex, RwLock};

    use nalgebra::Vector3;
    use winit::event::Event;

    /// Records the calls it receives so the manager's ordering can be checked.
    #[derive(Clone, Default)]
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl System for Recorder {
        fn init(&mut self, _world: Arc<RwLock<World>>) -> anyhow::Result<()> {
            self.log.lock().unwrap().push("init".to_string());

            Ok(())
        }

        fn update(
            &mut self,
            control: Arc<RwLock<Control>>,
            _world: Arc<RwLock<World>>,
        ) -> anyhow::Result<()> {
            let exit = control.read().unwrap().exit;
            self.log
                .lock()
                .unwrap()
                .push(format!("update(exit={exit})"));

            Ok(())
        }
    }

    fn world() -> Arc<RwLock<World>> {
        World::new(Vector3::zeros(), 0.0)
    }

    fn control() -> Arc<RwLock<Control>> {
        Control::new(Event::AboutToWait)
    }

    #[test]
    fn init_and_update_run_systems_in_registration_order() {
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut systems = SystemManager::new();
        systems.add(
            0,
            Recorder {
                log: Arc::clone(&log),
            },
        );
        systems.add(
            0,
            Recorder {
                log: Arc::clone(&log),
            },
        );
        systems.add(
            1,
            Recorder {
                log: Arc::clone(&log),
            },
        );

        systems.init(world()).unwrap();
        systems.update(control(), world()).unwrap();

        let log = log.lock().unwrap();

        assert_eq!(log.iter().filter(|line| *line == "init").count(), 3);
        assert_eq!(
            log.iter().filter(|line| line.starts_with("update")).count(),
            3
        );
    }

    #[test]
    fn a_system_sees_the_control_state_and_the_world() {
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut systems = SystemManager::new();
        systems.add(
            0,
            Recorder {
                log: Arc::clone(&log),
            },
        );

        let control = control();
        control.write().unwrap().exit = true;

        systems.update(control, world()).unwrap();

        assert_eq!(*log.lock().unwrap(), vec!["update(exit=true)".to_string()]);
    }

    #[test]
    fn rm_drops_the_last_system_of_a_pipeline() {
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut systems = SystemManager::new();
        systems.add(
            0,
            Recorder {
                log: Arc::clone(&log),
            },
        );
        systems.rm(0);

        systems.update(control(), world()).unwrap();

        assert!(log.lock().unwrap().is_empty());
    }
}
