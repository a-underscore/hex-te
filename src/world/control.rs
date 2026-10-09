use std::sync::{Arc, RwLock};

use winit::event::Event;

/// Input state handed to every [`System::update`](crate::world::System::update).
///
/// `event` is the winit event that triggered the frame; setting `exit` asks the
/// event loop to stop.
///
/// `hex` hardcodes `Event<()>` here. The type parameter is defaulted rather
/// than fixed so an application can thread its own winit user event through a
/// system.
pub struct Control<E: 'static = ()> {
    pub event: Event<E>,
    pub exit: bool,
}

impl<E: 'static> Control<E> {
    pub fn new(event: Event<E>) -> Arc<RwLock<Self>> {
        Arc::new(RwLock::new(Self { event, exit: false }))
    }
}
