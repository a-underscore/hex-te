pub mod component_manager;

pub use component_manager::ComponentManager;

use crate::world::Id;
use component_manager::ComponentManagerTrait;

use std::{
    any::TypeId,
    collections::{
        HashMap,
        hash_map::{Entry, Iter},
    },
    iter::FilterMap,
    sync::{Arc, RwLock},
};

/// Iterator over the ids of all active entities, as returned by
/// [`EntityManager::entities`].
pub type FilteredEntities<'a> =
    FilterMap<Iter<'a, Id, bool>, for<'b, 'c> fn((&'b Id, &'c bool)) -> Option<Id>>;

/// An entity-component store. Entities are plain [`Id`]s; components are held
/// per-type in [`ComponentManager`]s and fetched by type with
/// [`get_component`](EntityManager::get_component). An entity's `active` flag
/// controls whether [`entities`](EntityManager::entities) yields it.
pub struct EntityManager {
    free: Vec<Id>,
    entities: HashMap<Id, bool>,
    components: HashMap<TypeId, Box<dyn ComponentManagerTrait>>,
}

impl EntityManager {
    pub fn new() -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self {
            free: Default::default(),
            entities: Default::default(),
            components: Default::default(),
        }))
    }

    pub fn add(&mut self, active: bool) -> Id {
        let id = self.free.pop().unwrap_or(self.entities.len() as Id);

        self.entities.insert(id, active);

        id
    }

    pub fn rm(&mut self, eid: Id) {
        if self.entities.remove(&eid).is_some() {
            self.free.push(eid);

            for c in self.components.values_mut() {
                c.remove(eid);
            }
        }
    }

    pub fn is_active(&self, eid: Id) -> Option<bool> {
        self.entities.get(&eid).cloned()
    }

    /// Creates the component manager for `C` without attaching anything, so the
    /// type is present in the component map before its first component exists.
    ///
    /// Registering is idempotent: an existing manager is left alone.
    pub fn register<C: Send + Sync + 'static>(&mut self) {
        self.components
            .entry(TypeId::of::<C>())
            .or_insert(ComponentManager::<C>::new());
    }

    pub fn add_component<C: Send + Sync + 'static>(&mut self, eid: Id, component: Arc<RwLock<C>>) {
        let entry = self
            .components
            .entry(TypeId::of::<C>())
            .or_insert(ComponentManager::<C>::new());

        if let Some(manager) = entry.as_any_mut().downcast_mut::<ComponentManager<C>>()
            && self.entities.contains_key(&eid)
        {
            manager.components.insert(eid, component);
        }
    }

    pub fn rm_component<C: Send + Sync + 'static>(&mut self, eid: Id) {
        self.remove_component_generic(eid, TypeId::of::<C>());
    }

    pub fn get_component<C: Send + Sync + 'static>(&self, eid: Id) -> Option<Arc<RwLock<C>>> {
        self.get_component_manager::<C>()?.get(eid)
    }

    pub fn get_component_manager<C: Send + Sync + 'static>(&self) -> Option<&ComponentManager<C>> {
        self.components
            .get(&TypeId::of::<C>())?
            .as_any()
            .downcast_ref::<ComponentManager<C>>()
    }

    pub fn component_count(&self, eid: Id) -> usize {
        self.components
            .iter()
            .filter(|(_, c)| c.includes(eid))
            .count()
    }

    pub fn entities(&self) -> FilteredEntities<'_> {
        self.entities.iter().filter_map(|(e, a)| a.then_some(*e))
    }

    fn remove_component_generic(&mut self, eid: Id, cid: TypeId) {
        let Entry::Occupied(mut manager) = self.components.entry(cid) else {
            return;
        };

        if manager.get_mut().remove(eid) {
            manager.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EntityManager;

    use std::sync::{Arc, RwLock};

    /// Stand-in component: the store is type-erased, so any `Send + Sync` type
    /// works.
    struct Marker(u8);

    #[test]
    fn entities_are_listed_only_while_active() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        let active = em.add(true);
        let inactive = em.add(false);

        assert_eq!(em.is_active(active), Some(true));
        assert_eq!(em.is_active(inactive), Some(false));
        assert_eq!(em.entities().collect::<Vec<_>>(), vec![active]);
    }

    #[test]
    fn unknown_entities_report_nothing() {
        let shared = EntityManager::new();
        let em = shared.read().unwrap();

        assert_eq!(em.is_active(7), None);
        assert_eq!(em.component_count(7), 0);
    }

    #[test]
    fn removing_an_entity_frees_its_id_for_reuse() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        let first = em.add(true);
        em.rm(first);

        assert_eq!(em.is_active(first), None);
        assert_eq!(
            em.add(true),
            first,
            "the freed id should be handed out again"
        );
    }

    #[test]
    fn components_can_be_added_and_read_back() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        let eid = em.add(true);
        em.add_component(eid, Arc::new(RwLock::new(Marker(7))));

        let component = em.get_component::<Marker>(eid).expect("component");
        assert_eq!(component.read().unwrap().0, 7);
        assert_eq!(em.component_count(eid), 1);

        em.rm_component::<Marker>(eid);

        assert!(em.get_component::<Marker>(eid).is_none());
        assert_eq!(em.component_count(eid), 0);
        assert!(
            em.get_component_manager::<Marker>().is_none(),
            "a manager that ran empty is dropped"
        );
    }

    #[test]
    fn removing_an_entity_drops_its_components() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        let eid = em.add(true);
        em.add_component(eid, Arc::new(RwLock::new(Marker(1))));

        em.rm(eid);

        assert_eq!(em.component_count(eid), 0);
        assert!(em.get_component::<Marker>(eid).is_none());

        // Unlike `rm_component`, `rm` only calls `remove` on every manager; it
        // never drops the ones that ran empty. Kept as it is in `hex`: the
        // leftover entry is one `HashMap` slot holding no components.
        let manager = em.get_component_manager::<Marker>().expect("manager");
        assert!(manager.components.is_empty());
    }

    #[test]
    fn components_are_not_attached_to_unknown_entities() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        em.add_component(42, Arc::new(RwLock::new(Marker(1))));

        assert_eq!(em.component_count(42), 0);
        assert!(em.get_component::<Marker>(42).is_none());
    }

    #[test]
    fn components_are_shared_not_copied() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        let eid = em.add(true);
        let marker = Arc::new(RwLock::new(Marker(1)));
        em.add_component(eid, Arc::clone(&marker));

        // Writing through one handle is visible through the other, which is the
        // whole point of storing components behind `Arc<RwLock<_>>`.
        em.get_component::<Marker>(eid).unwrap().write().unwrap().0 = 9;
        assert_eq!(marker.read().unwrap().0, 9);
    }

    #[test]
    fn registering_a_component_type_creates_its_manager() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        assert!(em.get_component_manager::<Marker>().is_none());

        em.register::<Marker>();

        assert!(em.get_component_manager::<Marker>().is_some());
        assert_eq!(em.component_count(0), 0);
    }
}
