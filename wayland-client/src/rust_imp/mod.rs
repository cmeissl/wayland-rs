use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use downcast::Downcast;

use wayland_commons::map::ObjectMap;
use wayland_commons::wire::Message;
use wayland_commons::MessageGroup;

use {Interface, NewProxy, Proxy};

mod connection;
mod display;
mod proxy;
mod queues;

pub(crate) use self::display::DisplayInner;
pub(crate) use self::proxy::{NewProxyInner, ProxyInner};
pub(crate) use self::queues::EventQueueInner;

/// A handle to the object map internal to the lib state
///
/// This type is only usable by code generated by `wayland-scanner`, and is
/// not instanciable directly.
pub struct ProxyMap {
    map: Arc<Mutex<ObjectMap<self::proxy::ObjectMeta>>>,
    connection: Arc<Mutex<self::connection::Connection>>,
}

impl ProxyMap {
    pub(crate) fn make(
        map: Arc<Mutex<ObjectMap<self::proxy::ObjectMeta>>>,
        connection: Arc<Mutex<self::connection::Connection>>,
    ) -> ProxyMap {
        ProxyMap { map, connection }
    }

    /// Retrieve the Proxy corresponding to a given id
    pub fn get<I: Interface>(&mut self, id: u32) -> Option<Proxy<I>> {
        ProxyInner::from_id(id, self.map.clone(), self.connection.clone()).map(|object| {
            debug_assert!(I::NAME == "<anonymous>" || object.is_interface::<I>());
            Proxy::wrap(object)
        })
    }

    /// Create a new proxy for given id
    pub fn get_new<I: Interface>(&mut self, id: u32) -> Option<NewProxy<I>> {
        debug_assert!(
            self.map
                .lock()
                .unwrap()
                .find(id)
                .map(|obj| obj.is_interface::<I>())
                .unwrap_or(true)
        );
        NewProxyInner::from_id(id, self.map.clone(), self.connection.clone())
            .map(|object| NewProxy::wrap(object))
    }
}

pub(crate) trait Dispatcher: Downcast + Send {
    fn dispatch(&mut self, msg: Message, proxy: ProxyInner, map: &mut ProxyMap) -> Result<(), ()>;
}

mod dispatcher_impl {
    // this mod has for sole purpose to allow to silence these `dead_code` warnings...
    #![allow(dead_code)]
    use super::Dispatcher;
    impl_downcast!(Dispatcher);
}

pub(crate) struct ImplDispatcher<I: Interface, F: FnMut(I::Event, Proxy<I>) + 'static> {
    _i: ::std::marker::PhantomData<&'static I>,
    implementation: F,
}

// This unsafe impl is "technically wrong", but enforced by the fact that
// the Impl will only ever be called from the EventQueue, which is stuck
// on a single thread. The NewProxy::implement/implement_nonsend methods
// take care of ensuring that any non-Send impl is on the correct thread.
unsafe impl<I, F> Send for ImplDispatcher<I, F>
where
    I: Interface,
    F: FnMut(I::Event, Proxy<I>) + 'static,
    I::Event: MessageGroup<Map = ProxyMap>,
{}

impl<I, F> Dispatcher for ImplDispatcher<I, F>
where
    I: Interface,
    F: FnMut(I::Event, Proxy<I>) + 'static,
    I::Event: MessageGroup<Map = ProxyMap>,
{
    fn dispatch(&mut self, msg: Message, proxy: ProxyInner, map: &mut ProxyMap) -> Result<(), ()> {
        if ::std::env::var_os("WAYLAND_DEBUG").is_some() {
            println!(
                " <- {}@{}: {} {:?}",
                proxy.object.interface, proxy.id, proxy.object.events[msg.opcode as usize].name, msg.args
            );
        }
        let message = I::Event::from_raw(msg, map)?;
        if message.is_destructor() {
            proxy.object.meta.alive.store(false, Ordering::Release);
            {
                // cleanup the map as appropriate
                let mut map = proxy.map.lock().unwrap();
                let server_destroyed = map
                    .with(proxy.id, |obj| {
                        obj.meta.client_destroyed = true;
                        obj.meta.server_destroyed
                    }).unwrap_or(false);
                if server_destroyed {
                    map.remove(proxy.id);
                }
            }
            (self.implementation)(message, Proxy::<I>::wrap(proxy.clone()));
        } else {
            (self.implementation)(message, Proxy::<I>::wrap(proxy));
        }
        Ok(())
    }
}

pub(crate) unsafe fn make_dispatcher<I, F>(implementation: F) -> Arc<Mutex<Dispatcher + Send>>
where
    I: Interface,
    F: FnMut(I::Event, Proxy<I>) + 'static,
    I::Event: MessageGroup<Map = ProxyMap>,
{
    Arc::new(Mutex::new(ImplDispatcher {
        _i: ::std::marker::PhantomData,
        implementation,
    }))
}

pub(crate) fn default_dispatcher() -> Arc<Mutex<Dispatcher + Send>> {
    struct DefaultDisp;
    impl Dispatcher for DefaultDisp {
        fn dispatch(&mut self, _msg: Message, proxy: ProxyInner, _map: &mut ProxyMap) -> Result<(), ()> {
            eprintln!(
                "[wayland-client] Received an event for unimplemented object {}@{}.",
                proxy.object.interface, proxy.id
            );
            Err(())
        }
    }

    Arc::new(Mutex::new(DefaultDisp))
}
