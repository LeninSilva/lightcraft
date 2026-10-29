//! The platform boundary: everything the interface asks of the phone (pick a photo, store and
//! load blobs, save a file where the user can find it, share) is a [`HostRequest`]; answers come
//! back later as [`HostEvent`]s the app drains every frame. The WASM host implements it with the
//! browser and Capacitor APIs; tests use [`MemoryHost`].

use std::collections::{BTreeMap, VecDeque};

/// What the app asks the host to do. Every request is fire-and-forget; results arrive as events.
#[derive(Clone, Debug, PartialEq)]
pub enum HostRequest {
    /// Show the system photo picker (answer: [`HostEvent::Picked`] or [`HostEvent::PickCancelled`]).
    PickPhoto,
    /// Store a blob (answer: [`HostEvent::Failed`] only on error).
    Put { key: String, bytes: Vec<u8> },
    /// Load a blob (answer: [`HostEvent::Loaded`]).
    Get { key: String },
    /// Delete a blob (no answer).
    Delete { key: String },
    /// Save a file where the user finds it (the phone's Pictures/Documents; answer:
    /// [`HostEvent::Saved`] or [`HostEvent::Failed`]).
    SaveToDevice { name: String, bytes: Vec<u8> },
    /// Open the system share sheet with this file (answer: [`HostEvent::Failed`] only on error).
    Share { name: String, bytes: Vec<u8> },
}

/// What the host tells the app.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    Picked { name: String, bytes: Vec<u8> },
    PickCancelled,
    Loaded { key: String, bytes: Option<Vec<u8>> },
    Saved { name: String, location: String },
    Failed { what: String, message: String },
}

/// The platform.
pub trait Host {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> f64;
    fn send(&mut self, request: HostRequest);
    /// Events that arrived since the last call.
    fn poll(&mut self) -> Vec<HostEvent>;
}

/// An in-memory host for tests and native runs: storage is a map, pick answers come from a
/// queue the test fills, saved files are recorded.
#[derive(Default)]
pub struct MemoryHost {
    pub now: f64,
    pub store: BTreeMap<String, Vec<u8>>,
    /// Photos the next `PickPhoto` requests return, in order (empty: the pick is cancelled).
    pub picks: VecDeque<(String, Vec<u8>)>,
    pub saved: Vec<(String, Vec<u8>)>,
    pub shared: Vec<String>,
    pub requests: Vec<String>,
    events: VecDeque<HostEvent>,
}

impl Host for MemoryHost {
    fn now_ms(&self) -> f64 {
        self.now
    }
    fn send(&mut self, request: HostRequest) {
        self.now += 1.0;
        match request {
            HostRequest::PickPhoto => {
                self.requests.push("pick".into());
                match self.picks.pop_front() {
                    Some((name, bytes)) => self.events.push_back(HostEvent::Picked { name, bytes }),
                    None => self.events.push_back(HostEvent::PickCancelled),
                }
            }
            HostRequest::Put { key, bytes } => {
                self.requests.push(format!("put {key}"));
                self.store.insert(key, bytes);
            }
            HostRequest::Get { key } => {
                self.requests.push(format!("get {key}"));
                let bytes = self.store.get(&key).cloned();
                self.events.push_back(HostEvent::Loaded { key, bytes });
            }
            HostRequest::Delete { key } => {
                self.requests.push(format!("delete {key}"));
                self.store.remove(&key);
            }
            HostRequest::SaveToDevice { name, bytes } => {
                self.requests.push(format!("save {name}"));
                self.events.push_back(HostEvent::Saved { name: name.clone(), location: format!("Pictures/LightCraft/{name}") });
                self.saved.push((name, bytes));
            }
            HostRequest::Share { name, .. } => {
                self.requests.push(format!("share {name}"));
                self.shared.push(name);
            }
        }
    }
    fn poll(&mut self) -> Vec<HostEvent> {
        self.events.drain(..).collect()
    }
}
