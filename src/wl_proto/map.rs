//! Wayland objects map
use super::protocol::Interface;

use std::collections::HashMap;

/// Limit separating server-created from client-created objects IDs in the namespace
pub const SERVER_ID_LIMIT: u32 = 0xFF00_0000;

/// The representation of a protocol object
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Object<Data> {
    /// Interface name of this object
    pub interface: &'static Interface,
    /// Version of this object
    pub version: u32,
    /// ObjectData associated to this object (ex: its event queue client side)
    pub data: Data,
}

/// A holder for the object store of a connection
///
/// Keeps track of which object id is associated to which
/// interface object, and which is currently unused.
#[derive(Debug, Default)]
pub struct ObjectMap<Data> {
    client_objects: HashMap<u32, Object<Data>>,
    server_objects: HashMap<u32, Object<Data>>,
}

impl<Data: Clone> ObjectMap<Data> {
    /// Create a new empty object map
    pub fn new() -> Self {
        Self {
            client_objects: HashMap::new(),
            server_objects: HashMap::new(),
        }
    }

    /// Find an object in the store
    pub fn find(&self, id: u32) -> Option<Object<Data>> {
        if id == 0 {
            None
        } else if id >= SERVER_ID_LIMIT {
            self.server_objects.get(&(id - SERVER_ID_LIMIT)).cloned()
        } else {
            self.client_objects.get(&(id - 1)).cloned()
        }
    }

    /// Remove an object from the store
    ///
    /// Does nothing if the object didn't previously exists
    pub fn remove(&mut self, id: u32) {
        if id == 0 {
            // nothing
        } else if id >= SERVER_ID_LIMIT {
            self.server_objects.remove(&(id - SERVER_ID_LIMIT));
        } else {
            self.client_objects.remove(&(id - 1));
        }
    }

    /// Insert given object for given id
    ///
    /// Can fail if the requested id is not the next free id of this store.
    /// (In which case this is a protocol error)
    pub fn insert_at(&mut self, id: u32, object: Object<Data>) -> Result<(), ()> {
        if id == 0 {
            Err(())
        } else if id >= SERVER_ID_LIMIT {
            self.server_objects.insert(id - SERVER_ID_LIMIT, object);
            Ok(())
        } else {
            self.client_objects.insert(id - 1, object);
            Ok(())
        }
    }

    /*
    /// Mutably access an object of the map
    pub fn with<T, F: FnOnce(&mut Object<Data>) -> T>(&mut self, id: u32, f: F) -> Result<T, ()> {
        if id == 0 {
            Err(())
        } else if id >= SERVER_ID_LIMIT {
            if let Some(&mut Some(ref mut obj)) =
                self.server_objects.get_mut((id - SERVER_ID_LIMIT) as usize)
            {
                Ok(f(obj))
            } else {
                Err(())
            }
        } else if let Some(&mut Some(ref mut obj)) = self.client_objects.get_mut((id - 1) as usize)
        {
            Ok(f(obj))
        } else {
            Err(())
        }
    }

    pub fn all_objects(&self) -> impl Iterator<Item = (u32, &Object<Data>)> {
        let client_side_iter = self
            .client_objects
            .iter()
            .enumerate()
            .flat_map(|(idx, obj)| obj.as_ref().map(|obj| (idx as u32 + 1, obj)));

        let server_side_iter = self
            .server_objects
            .iter()
            .enumerate()
            .flat_map(|(idx, obj)| obj.as_ref().map(|obj| (idx as u32 + SERVER_ID_LIMIT, obj)));

        client_side_iter.chain(server_side_iter)
    }
    */
}
