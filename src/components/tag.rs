use crate::Id;
use crate::world::EntityManager;

use std::sync::{Arc, RwLock};

/// A string name attached to an entity, mainly for lookup by [`Tag::find`].
#[derive(Clone)]
pub struct Tag(pub String);

impl Tag {
    pub fn new<S>(t: S) -> Arc<RwLock<Self>>
    where
        S: Into<String>,
    {
        Arc::new(RwLock::new(Self(t.into())))
    }

    pub fn find(&self, em: &EntityManager) -> Option<Id> {
        em.entities().find_map(|e| {
            em.get_component::<Tag>(e)
                .and_then(|t| (self.0 == t.read().unwrap().0).then_some(e))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Tag;
    use crate::world::EntityManager;

    #[test]
    fn find_locates_the_entity_carrying_that_name() {
        let shared = EntityManager::new();
        let mut em = shared.write().unwrap();

        let untagged = em.add(true);
        let named = em.add(true);
        em.add_component(named, Tag::new("player"));

        let tag = em.get_component::<Tag>(named).expect("tag");
        let tag = tag.read().unwrap();

        assert_eq!(tag.find(&em), Some(named));
        assert_ne!(tag.find(&em), Some(untagged));
        assert_eq!(Tag("missing".to_string()).find(&em), None);
    }
}
