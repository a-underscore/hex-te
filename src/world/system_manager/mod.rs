pub mod system;

pub use system::System;

use crate::world::{Control, Id, World};

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

/// Ordered groups of [`System`]s, one pipeline per id.
///
/// Every system sits behind its own handle rather than in a plain `Box`, which
/// is what `hex` did for its thread pool: it makes the manager cheap to clone,
/// so a caller can take a snapshot and run the systems *without* holding the
/// manager's lock. A system that reaches back into the manager — a Python one
/// usually does — then works instead of deadlocking on its own lock. There is
/// no thread pool here yet, so the systems run one after another on the calling
/// thread.
pub struct SystemManager<E: 'static = ()> {
    pipelines: HashMap<Id, Pipeline<E>>,
}

/// The systems of one pipeline, in the order they were added.
type Pipeline<E> = Vec<Arc<Mutex<Box<dyn System<E>>>>>;

/// Cloning the manager clones the handles, not the systems: the copy runs the
/// same behaviour, but needs none of the manager's own lock to do it.
///
/// Written by hand because a derived `Clone` would demand `E: Clone`.
impl<E: 'static> Clone for SystemManager<E> {
    fn clone(&self) -> Self {
        Self {
            pipelines: self.pipelines.clone(),
        }
    }
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
        self.pipelines
            .entry(pid)
            .or_default()
            .push(Arc::new(Mutex::new(s)));
    }

    pub fn add<S: System<E>>(&mut self, pid: Id, s: S) {
        self.add_gen(pid, Box::new(s));
    }

    pub fn rm(&mut self, pid: Id) {
        if let Some(pipeline) = self.pipelines.get_mut(&pid) {
            pipeline.pop();
        }
    }

    /// How many systems are registered, across every pipeline.
    pub fn system_count(&self) -> usize {
        self.pipelines.values().map(Vec::len).sum()
    }

    /// How many systems each pipeline holds, by id.
    ///
    /// A caller that runs a config file — which is free to register systems of
    /// its own — takes one of these before and after, so it can tell what the
    /// file added and take it back when it is next read.
    pub fn pipeline_counts(&self) -> HashMap<Id, usize> {
        self.pipelines
            .iter()
            .map(|(pid, pipeline)| (*pid, pipeline.len()))
            .collect()
    }

    /// Runs every system's `init`, in snapshot order.
    pub fn init(&self, world: Arc<RwLock<World<E>>>) -> anyhow::Result<()> {
        for system in self.snapshot() {
            system.lock().unwrap().init(Arc::clone(&world))?;
        }

        Ok(())
    }

    /// Runs every system's `update`, in snapshot order.
    pub fn update(
        &self,
        control: Arc<RwLock<Control<E>>>,
        world: Arc<RwLock<World<E>>>,
    ) -> anyhow::Result<()> {
        for system in self.snapshot() {
            system
                .lock()
                .unwrap()
                .update(Arc::clone(&control), Arc::clone(&world))?;
        }

        Ok(())
    }

    /// Runs the `update` of one pipeline's systems, leaving the other pipelines
    /// alone.
    ///
    /// The renderer wants this: a render function is registered in a pipeline of
    /// its own so that it runs once per frame, at the point the frame is drawn,
    /// rather than once per event alongside the app's own systems.
    pub fn update_pipeline(
        &self,
        pid: Id,
        control: Arc<RwLock<Control<E>>>,
        world: Arc<RwLock<World<E>>>,
    ) -> anyhow::Result<()> {
        for system in self.pipeline(pid) {
            system
                .lock()
                .unwrap()
                .update(Arc::clone(&control), Arc::clone(&world))?;
        }

        Ok(())
    }

    /// Handles to the systems of every pipeline, ready to run.
    ///
    /// The pipelines are walked in an unspecified order; the systems of one
    /// pipeline run in the order they were added.
    fn snapshot(&self) -> Vec<Arc<Mutex<Box<dyn System<E>>>>> {
        self.pipelines.values().flatten().cloned().collect()
    }

    /// Handles to the systems of one pipeline, in the order they were added.
    ///
    /// Taken out of the map rather than run from underneath it, for the same
    /// reason [`snapshot`](Self::snapshot) is: a system may reach back into the
    /// manager while it runs.
    fn pipeline(&self, pid: Id) -> Vec<Arc<Mutex<Box<dyn System<E>>>>> {
        self.pipelines.get(&pid).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::{System, SystemManager};
    use crate::world::{Control, World};

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
    fn one_pipeline_can_be_run_on_its_own() {
        // What the renderer does: the systems of the render pipeline run once
        // per frame, and the app's own event systems do not run again with it.
        let log = Arc::new(Mutex::new(Vec::new()));
        let record = |systems: &mut SystemManager, pid| {
            systems.add(
                pid,
                Recorder {
                    log: Arc::clone(&log),
                },
            );
        };

        let mut systems = SystemManager::new();
        record(&mut systems, 0);
        record(&mut systems, 1);

        systems.update_pipeline(1, control(), world()).unwrap();

        assert_eq!(*log.lock().unwrap(), vec!["update(exit=false)".to_owned()]);

        // A pipeline nothing was added to costs nothing and runs nothing.
        systems.update_pipeline(2, control(), world()).unwrap();

        assert_eq!(log.lock().unwrap().len(), 1);
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

    #[test]
    fn pipeline_counts_say_what_each_pipeline_holds() {
        // What a config loader takes before and after evaluating a file, so it
        // can tell the systems the file added from the app's own.
        let mut systems = SystemManager::new();
        systems.add(0, Recorder::default());
        systems.add(0, Recorder::default());
        systems.add(2, Recorder::default());

        let counts = systems.pipeline_counts();

        assert_eq!(counts.get(&0), Some(&2));
        assert_eq!(counts.get(&2), Some(&1));
        assert_eq!(counts.get(&1), None, "a pipeline nothing was added to");
        assert_eq!(counts.values().sum::<usize>(), systems.system_count());
    }
}
