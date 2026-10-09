pub mod component_manager_trait;

pub use component_manager_trait::ComponentManagerTrait;

use crate::world::Id;

use std::{
    any::Any,
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub struct ComponentManager<C: Send + Sync + 'static> {
    pub(crate) components: HashMap<Id, Arc<RwLock<C>>>,
}

impl<C: Send + Sync + 'static> ComponentManager<C> {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            components: HashMap::new(),
        })
    }
}

impl<C: Send + Sync + 'static> ComponentManager<C> {
    pub fn get(&self, eid: Id) -> Option<Arc<RwLock<C>>> {
        self.components.get(&eid).cloned()
    }
}

impl<C: Send + Sync + 'static> ComponentManagerTrait for ComponentManager<C> {
    fn as_any(&self) -> &(dyn Any + Send + Sync + 'static) {
        self
    }

    fn as_any_mut(&mut self) -> &mut (dyn Any + Send + Sync + 'static) {
        self
    }

    fn includes(&self, eid: Id) -> bool {
        self.components.contains_key(&eid)
    }

    fn remove(&mut self, eid: Id) -> bool {
        self.components.remove(&eid);

        self.components.is_empty()
    }
}
