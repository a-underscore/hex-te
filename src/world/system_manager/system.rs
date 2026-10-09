use crate::world::{Control, World};

use std::sync::{Arc, RwLock};

/// A unit of per-frame behaviour.
///
/// Systems are registered in a [`SystemManager`](crate::world::SystemManager):
/// `init` runs once when the application starts, `update` runs for every event
/// the event loop dispatches.
///
/// `hex` passed a `Context` (device, swapchain, thread pool) alongside the
/// world. There is no context here, and everything a system needs is expected
/// to be reachable through the world, so it receives just the event
/// ([`Control`]) and the world.
pub trait System<E: 'static = ()>: Send + Sync + 'static {
    fn init(&mut self, _world: Arc<RwLock<World<E>>>) -> anyhow::Result<()> {
        Ok(())
    }

    fn update(
        &mut self,
        _control: Arc<RwLock<Control<E>>>,
        _world: Arc<RwLock<World<E>>>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}
