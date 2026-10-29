//! The platform boundary: everything the interface asks of the phone (pick a photo, store and
//! load blobs, save a file where the user can find it, share) is a [`HostRequest`]; answers come
//! back later as [`HostEvent`]s the app drains every frame. [`WebHost`] implements it with the
//! browser (file picker, OPFS with an IndexedDB fallback, download export; see
//! [`storage`]); sharing is not wired yet and answers [`HostEvent::Failed`]. Tests use
//! [`MemoryHost`].

#[cfg(target_arch = "wasm32")]
mod storage;

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

/// A download name with no directory parts: the name the app asks to save, stripped of any
/// `/` or `\` prefix, so a hostile or mistaken name cannot escape the download folder.
/// Testable natively; used by [`WebHost`]'s download export.
fn safe_file_name(name: &str) -> &str {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("photo");
    if name.is_empty() { "photo" } else { name }
}

#[cfg(target_arch = "wasm32")]
mod web_host {
    use super::{Host, HostEvent, HostRequest, safe_file_name, storage::Backend};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::JsFuture;

    /// Whether the storage backend is ready yet.
    enum BackendState {
        Opening,
        Ready(Backend),
        Failed(String),
    }

    /// Shared state: the event queue async tasks push into, plus the egui context to repaint
    /// when an answer arrives between frames.
    #[derive(Clone)]
    struct Shared {
        events: Rc<RefCell<VecDeque<HostEvent>>>,
        ctx: egui::Context,
    }

    impl Shared {
        fn push(&self, event: HostEvent) {
            self.events.borrow_mut().push_back(event);
            self.ctx.request_repaint();
        }
    }

    /// The browser host: the file picker, OPFS/IndexedDB storage, and download export.
    /// Storage requests sent before the backend opens are queued and flushed in order once
    /// it is ready; if it fails to open, every queued and later storage request answers
    /// [`HostEvent::Failed`]. `Share` is not wired yet and always answers `Failed`.
    pub struct WebHost {
        shared: Shared,
        backend: Rc<RefCell<BackendState>>,
        pending: Rc<RefCell<VecDeque<HostRequest>>>,
    }

    impl WebHost {
        /// Start opening storage in the background; `ctx` is repainted when answers arrive.
        pub fn new(ctx: egui::Context) -> WebHost {
            let shared = Shared { events: Rc::new(RefCell::new(VecDeque::new())), ctx };
            let backend: Rc<RefCell<BackendState>> = Rc::new(RefCell::new(BackendState::Opening));
            let pending: Rc<RefCell<VecDeque<HostRequest>>> = Rc::new(RefCell::new(VecDeque::new()));
            let (b2, p2, s2) = (backend.clone(), pending.clone(), shared.clone());
            wasm_bindgen_futures::spawn_local(async move {
                match Backend::open().await {
                    Ok(b) => {
                        log::info!("lightcraft-mobile: storage backend {}", b.kind());
                        *b2.borrow_mut() = BackendState::Ready(b.clone());
                        for req in p2.borrow_mut().drain(..) {
                            let (b3, s3) = (b.clone(), s2.clone());
                            wasm_bindgen_futures::spawn_local(async move {
                                run_storage(b3, req, s3).await;
                            });
                        }
                    }
                    Err(e) => {
                        log::warn!("lightcraft-mobile: storage unavailable: {e}");
                        *b2.borrow_mut() = BackendState::Failed(e.clone());
                        for req in p2.borrow_mut().drain(..) {
                            let _ = req;
                            s2.push(HostEvent::Failed { what: "storage".into(), message: e.clone() });
                        }
                    }
                }
            });
            WebHost { shared, backend, pending }
        }

        fn storage(&mut self, request: HostRequest) {
            match &*self.backend.borrow() {
                BackendState::Ready(b) => {
                    let (b, s) = (b.clone(), self.shared.clone());
                    wasm_bindgen_futures::spawn_local(async move {
                        run_storage(b, request, s).await;
                    });
                }
                BackendState::Opening => self.pending.borrow_mut().push_back(request),
                BackendState::Failed(message) => self.shared.push(HostEvent::Failed { what: "storage".into(), message: message.clone() }),
            }
        }
    }

    impl Host for WebHost {
        fn now_ms(&self) -> f64 {
            js_sys::Date::now()
        }

        fn send(&mut self, request: HostRequest) {
            match request {
                HostRequest::PickPhoto => pick_photo(self.shared.clone()),
                HostRequest::Put { .. } | HostRequest::Get { .. } | HostRequest::Delete { .. } => self.storage(request),
                HostRequest::SaveToDevice { name, bytes } => {
                    let clean = safe_file_name(&name).to_string();
                    match download(&name, &bytes) {
                        Ok(()) => self.shared.push(HostEvent::Saved { name: clean.clone(), location: format!("Downloads/{clean}") }),
                        Err(message) => self.shared.push(HostEvent::Failed { what: "save".into(), message }),
                    }
                }
                HostRequest::Share { name, .. } => self.shared.push(HostEvent::Failed {
                    what: "share".into(),
                    message: format!("Sharing {name} is not available in this build yet; save the file first."),
                }),
            }
        }

        fn poll(&mut self) -> Vec<HostEvent> {
            self.shared.events.borrow_mut().drain(..).collect()
        }
    }

    /// Run one storage request against an open backend, pushing the answer (if any).
    async fn run_storage(backend: Backend, request: HostRequest, shared: Shared) {
        match request {
            HostRequest::Put { key, bytes } => {
                if let Err(e) = backend.write(&key, &bytes).await {
                    shared.push(HostEvent::Failed { what: "storage".into(), message: format!("storing {key}: {e}") });
                }
            }
            HostRequest::Get { key } => match backend.read(&key).await {
                Ok(bytes) => shared.push(HostEvent::Loaded { key, bytes }),
                Err(e) => shared.push(HostEvent::Failed { what: "storage".into(), message: format!("loading {key}: {e}") }),
            },
            HostRequest::Delete { key } => {
                if let Err(e) = backend.remove(&key).await {
                    shared.push(HostEvent::Failed { what: "storage".into(), message: format!("deleting {key}: {e}") });
                }
            }
            _ => {}
        }
    }

    /// Show the browser's open dialog; the picked file is read asynchronously and answers
    /// [`HostEvent::Picked`]. Cancelling the dialog fires no event in most browsers, so the
    /// app simply waits (the same limitation as the desktop web app's picker).
    fn pick_photo(shared: Shared) {
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
        let Ok(input) = doc.create_element("input").map(|e| e.unchecked_into::<web_sys::HtmlInputElement>()) else { return };
        input.set_type("file");
        input.set_multiple(false);
        input.set_accept("image/*,.jpg,.jpeg,.png,.webp,.tif,.tiff,.dng,.cr2,.cr3,.nef,.arw,.orf,.rw2,.raf,.pef,.heic,.heif,.avif");
        let inp = input.clone();
        let on_change = Closure::<dyn FnMut()>::new(move || {
            let Some(files) = inp.files() else {
                shared.push(HostEvent::PickCancelled);
                return;
            };
            if files.length() == 0 {
                shared.push(HostEvent::PickCancelled);
                return;
            }
            if let Some(f) = files.get(0) {
                let s2 = shared.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    match JsFuture::from(f.array_buffer()).await {
                        Ok(buf) => s2.push(HostEvent::Picked { name: f.name(), bytes: js_sys::Uint8Array::new(&buf).to_vec() }),
                        Err(e) => s2.push(HostEvent::Failed { what: "pick".into(), message: format!("reading {}: {e:?}", f.name()) }),
                    }
                });
            }
        });
        input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        on_change.forget();
        input.click();
    }

    /// Offer `bytes` as a browser download named `name` (mirrors the desktop web app's
    /// export). In the Capacitor WebView this lands in the phone's Downloads folder; whether
    /// the WebView surfaces the download is a phone-test item (see PLATFORM-NOTES.md).
    fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
        let e = |e: JsValue| format!("download failed: {e:?}");
        let clean = safe_file_name(name);
        let arr = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let opts = web_sys::BlobPropertyBag::new();
        let mime = if clean.ends_with(".jpg") || clean.ends_with(".jpeg") {
            "image/jpeg"
        } else if clean.ends_with(".png") {
            "image/png"
        } else {
            "application/octet-stream"
        };
        opts.set_type(mime);
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&arr, &opts).map_err(e)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(e)?;
        let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
        let a: web_sys::HtmlAnchorElement = doc.create_element("a").map_err(e)?.unchecked_into();
        a.set_href(&url);
        a.set_download(clean);
        a.click();
        let revoke = Closure::once_into_js(move || {
            let _ = web_sys::Url::revoke_object_url(&url);
        });
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 5000);
        }
        log::info!("exported {clean} ({} bytes)", bytes.len());
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
pub use web_host::WebHost;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_file_name_strips_directories() {
        assert_eq!(safe_file_name("edited.jpg"), "edited.jpg");
        assert_eq!(safe_file_name("Pictures/LightCraft/edited.jpg"), "edited.jpg");
        assert_eq!(safe_file_name("C:\\Users\\x\\edited.jpg"), "edited.jpg");
        assert_eq!(safe_file_name(""), "photo");
        assert_eq!(safe_file_name("a/"), "photo");
    }

    #[test]
    fn memory_host_round_trip() {
        let mut host = MemoryHost::default();
        host.send(HostRequest::Put { key: "library/index.json".into(), bytes: vec![1, 2] });
        host.send(HostRequest::Get { key: "library/index.json".into() });
        assert_eq!(host.poll(), vec![HostEvent::Loaded { key: "library/index.json".into(), bytes: Some(vec![1, 2]) }]);
    }
}
