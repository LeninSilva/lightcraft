//! The platform boundary: everything the interface asks of the phone (pick a photo, store and
//! load blobs, save a file where the user can find it, share) is a [`HostRequest`]; answers come
//! back later as [`HostEvent`]s the app drains every frame. [`WebHost`] implements it with the
//! browser (file picker, OPFS with an IndexedDB fallback, download export, Web Share
//! with a download fallback; see [`crate::ui::storage`]). Inside a Capacitor shell
//! (the phone app) saving and sharing instead go through the Capacitor
//! Filesystem and Share plugins, because Android WebView implements
//! neither the anchor download nor `navigator.share`. Tests use [`MemoryHost`].

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

/// Whether sharing a file needs the download fallback: the Web Share API is absent, or
/// the browser says it cannot share files. Pure, so it is tested natively; the browser
/// check itself lives in [`WebHost`]'s share flow.
fn should_fallback_to_download(share_api_present: bool, can_share_files: bool) -> bool {
    !share_api_present || !can_share_files
}

/// The message for a failed share, from the browser error's `name`. A cancelled share
/// is the user's own choice and says so; any other failure names its kind, and an
/// error with no name falls back to the supplied text. `Failed` events for sharing
/// always carry one of these, never a bare "not available".
fn share_failure_message(error_name: &str, fallback: &str) -> String {
    match error_name {
        "AbortError" => "Sharing was cancelled.".to_string(),
        "" => fallback.to_string(),
        name => format!("Sharing failed ({name})."),
    }
}

/// Base64 (standard alphabet, padded) encoding of `data`, for the Capacitor
/// Filesystem plugin, which takes binary file contents as a base64 string.
/// Pure, so it is tested natively. Implemented here rather than gained as a
/// dependency for a thirty-line encoder.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Whether a plugin error's name or message says the user cancelled, in
/// either spelling the bridges use ("AbortError", "Share canceled").
/// Pure, so it is tested natively.
fn is_cancelled(name: &str, message: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let message = message.to_ascii_lowercase();
    name.contains("cancel") || name.contains("abort") || message.contains("cancel")
}

/// Human-readable text for a plugin error: its message, else its name,
/// else a fallback. Pure, so it is tested natively.
fn plugin_error_text(name: &str, message: &str) -> String {
    if !message.is_empty() {
        message.to_string()
    } else if !name.is_empty() {
        name.to_string()
    } else {
        "unknown error".to_string()
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
    use super::{Host, HostEvent, HostRequest, base64_encode, is_cancelled, plugin_error_text, safe_file_name, share_failure_message, should_fallback_to_download};
    use crate::library::INDEX_KEY;
    use crate::ui::storage::Backend;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;
    use js_sys::Reflect;
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

    /// The browser host: the file picker, OPFS/IndexedDB storage, download export, and
    /// the Web Share API with a download fallback. Inside a Capacitor shell, saving
    /// and sharing go through the Capacitor Filesystem and Share plugins instead:
    /// Android WebView implements neither the anchor download nor `navigator.share`,
    /// so those browser paths must never be used there, and no success is reported
    /// until the plugin call actually succeeds.
    /// Storage requests are queued and run by one sequential worker, in queue
    /// order, so a stale index write can never land after a newer one; requests
    /// sent before the backend opens wait in the same queue. If the backend
    /// fails to open, every queued and later storage request answers
    /// [`HostEvent::Failed`]. `Share` opens the system share sheet when the browser
    /// supports sharing files; otherwise it downloads the file instead and answers
    /// [`HostEvent::Saved`], so the user still gets the file.
    // How long storage may take to open before the app reports the failure
    // instead of showing "Loading your library." forever (M4).
    const OPEN_TIMEOUT_MS: f64 = 30_000.0;

    pub struct WebHost {
        shared: Shared,
        backend: Rc<RefCell<BackendState>>,
        pending: Rc<RefCell<VecDeque<HostRequest>>>,
        pumping: Rc<Cell<bool>>,
        /// The one file-picker input that may be waiting for an answer, if
        /// any. At most one exists: a new pick removes the previous one.
        pending_pick: Rc<RefCell<Option<web_sys::HtmlInputElement>>>,
        /// Wall-clock time, in milliseconds, when the current open attempt started.
        opened_at_ms: Rc<Cell<f64>>,
        /// Generation of the current open attempt. Bumped by a retry so a
        /// late opener from a superseded attempt cannot resolve or fail
        /// the new one.
        open_generation: Rc<Cell<u64>>,
    }

    impl WebHost {
        /// Start opening storage in the background; `ctx` is repainted when answers arrive.
        pub fn new(ctx: egui::Context) -> WebHost {
            let shared = Shared { events: Rc::new(RefCell::new(VecDeque::new())), ctx };
            let backend: Rc<RefCell<BackendState>> = Rc::new(RefCell::new(BackendState::Opening));
            let pending: Rc<RefCell<VecDeque<HostRequest>>> = Rc::new(RefCell::new(VecDeque::new()));
            let pumping: Rc<Cell<bool>> = Rc::new(Cell::new(false));
            let opened_at_ms = Rc::new(Cell::new(js_sys::Date::now()));
            let open_generation = Rc::new(Cell::new(0u64));
            let pending_pick = Rc::new(RefCell::new(None));
            start_open(&backend, &pending, &shared, &pumping, open_generation.clone(), 0);
            WebHost { shared, backend, pending, pumping, pending_pick, opened_at_ms, open_generation }
        }

        /// Retry opening storage after a failure (including a timeout).
        /// Only a Failed backend retries: a backend that is still opening
        /// keeps its own opener, and a Ready backend needs none. The
        /// generation is bumped first, so a late opener from the failed
        /// attempt cannot resolve or fail this new attempt (see
        /// [`start_open`]). The initial index load is re-queued ahead of
        /// the request that triggered the retry, because the UI sent that
        /// Get once at startup and will not send it again; the queued
        /// request itself runs after the backend is ready.
        /// The UI reaches this through any storage request it sends: there
        /// is deliberately no new `HostRequest` variant, because adding one
        /// would break the exhaustive matches in `MemoryHost` and the test
        /// host in `ui/mod.rs`, which this change may not touch. Tapping a
        /// photo, picking a photo, or saving retries; doing nothing does
        /// not retry.
        fn retry_open(&mut self) {
            if !matches!(*self.backend.borrow(), BackendState::Failed(_)) {
                return;
            }
            let next = self.open_generation.get() + 1;
            self.open_generation.set(next);
            *self.backend.borrow_mut() = BackendState::Opening;
            self.opened_at_ms.set(js_sys::Date::now());
            start_open(&self.backend, &self.pending, &self.shared, &self.pumping, self.open_generation.clone(), next);
            self.pending.borrow_mut().push_back(HostRequest::Get { key: INDEX_KEY.to_string() });
        }

        fn storage(&mut self, request: HostRequest) {
            if matches!(*self.backend.borrow(), BackendState::Failed(_)) {
                self.retry_open();
            }
            self.pending.borrow_mut().push_back(request);
            kick_worker(&self.backend, &self.pending, &self.shared, &self.pumping);
        }
    }

    /// Run queued storage requests one at a time, in queue order, awaiting each
    /// before taking the next. Only one worker runs at a time (`pumping`
    /// guards that); requests queued while it runs are picked up by its loop,
    /// and a request queued after it stops restarts it through `kick_worker`.
    async fn storage_worker(
        backend: Backend,
        pending: Rc<RefCell<VecDeque<HostRequest>>>,
        shared: Shared,
        pumping: Rc<Cell<bool>>,
    ) {
        loop {
            let Some(req) = pending.borrow_mut().pop_front() else {
                // Queue empty: stop, but re-check once after clearing the flag
                // so a request queued in between is not stranded.
                pumping.set(false);
                if pending.borrow().is_empty() {
                    return;
                }
                pumping.set(true);
                continue;
            };
            run_storage(backend.clone(), req, shared.clone()).await;
        }
    }

    /// Start the single storage worker if it is not already running and the
    /// backend is ready. Requests queued while the backend is still opening
    /// wait; the opener kicks the worker once the backend is ready.
    fn kick_worker(
        backend: &Rc<RefCell<BackendState>>,
        pending: &Rc<RefCell<VecDeque<HostRequest>>>,
        shared: &Shared,
        pumping: &Rc<Cell<bool>>,
    ) {
        if pumping.get() {
            return;
        }
        let ready = match &*backend.borrow() {
            BackendState::Ready(b) => b.clone(),
            _ => return,
        };
        pumping.set(true);
        let (b, p, s, pump) = (ready, pending.clone(), shared.clone(), pumping.clone());
        wasm_bindgen_futures::spawn_local(async move {
            storage_worker(b, p, s, pump).await;
        });
    }

    /// Open the storage backend in the background. Only the opener whose
    /// generation is still current and whose backend is still Opening may
    /// resolve or fail it: a timed-out opener must not resurrect a backend
    /// the watchdog already failed, and a superseded opener (one whose
    /// attempt a retry replaced) must not fail the retry's attempt.
    fn start_open(
        backend: &Rc<RefCell<BackendState>>,
        pending: &Rc<RefCell<VecDeque<HostRequest>>>,
        shared: &Shared,
        pumping: &Rc<Cell<bool>>,
        generations: Rc<Cell<u64>>,
        generation: u64,
    ) {
        let (b2, p2, s2, pump2) = (backend.clone(), pending.clone(), shared.clone(), pumping.clone());
        wasm_bindgen_futures::spawn_local(async move {
            match Backend::open().await {
                Ok(b) => {
                    log::info!("lightcraft-mobile: storage backend {}", b.kind());
                    let current = generations.get() == generation && matches!(*b2.borrow(), BackendState::Opening);
                    if current {
                        *b2.borrow_mut() = BackendState::Ready(b);
                        kick_worker(&b2, &p2, &s2, &pump2);
                    }
                }
                Err(e) => {
                    log::warn!("lightcraft-mobile: storage unavailable: {e}");
                    let current = generations.get() == generation && matches!(*b2.borrow(), BackendState::Opening);
                    if current {
                        *b2.borrow_mut() = BackendState::Failed(e.clone());
                        for req in p2.borrow_mut().drain(..) {
                            let _ = req;
                            s2.push(HostEvent::Failed { what: "storage".into(), message: e.clone() });
                        }
                    }
                }
            }
        });
    }

    impl Host for WebHost {
        fn now_ms(&self) -> f64 {
            js_sys::Date::now()
        }

        fn send(&mut self, request: HostRequest) {
            match request {
                HostRequest::PickPhoto => pick_photo(self.shared.clone(), self.pending_pick.clone()),
                HostRequest::Put { .. } | HostRequest::Get { .. } | HostRequest::Delete { .. } => self.storage(request),
                HostRequest::SaveToDevice { name, bytes } => {
                    // Saving is asynchronous in the Capacitor shell (a plugin
                    // promise), so it runs as a task like Share, and answers
                    // only from the plugin's real result.
                    let shared = self.shared.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        save_to_device(shared, name, bytes).await;
                    });
                }
                HostRequest::Share { name, bytes } => {
                    let shared = self.shared.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        share_photo(shared, name, bytes).await;
                    });
                }
            }
        }

        fn poll(&mut self) -> Vec<HostEvent> {
            // Opening has no async timeout of its own (no timer plumbing in
            // this crate), so the frame loop watches it: if storage still has
            // not opened after OPEN_TIMEOUT_MS, fail it visibly instead of
            // leaving the library loading forever. A late opener is ignored
            // by the still-opening guard in `new`.
            let still_opening = matches!(*self.backend.borrow(), BackendState::Opening);
            if still_opening && js_sys::Date::now() - self.opened_at_ms.get() > OPEN_TIMEOUT_MS {
                let message = "storage did not open in time".to_string();
                *self.backend.borrow_mut() = BackendState::Failed(message.clone());
                for _ in self.pending.borrow_mut().drain(..) {
                    self.shared.push(HostEvent::Failed { what: "storage".into(), message: message.clone() });
                }
            }
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

    /// The (`name`, `message`) properties of a JS rejection value, or empty
    /// strings when it has neither (a bare string rejection is handled by
    /// [`plugin_error`], which checks that first).
    fn js_error_parts(e: &JsValue) -> (String, String) {
        let name = Reflect::get(e, &"name".into()).ok().and_then(|n| n.as_string()).unwrap_or_default();
        let message = Reflect::get(e, &"message".into()).ok().and_then(|m| m.as_string()).unwrap_or_default();
        (name, message)
    }

    /// Human-readable text for a JS rejection value: a bare string rejection
    /// as-is, else its message, else its name, else a fallback.
    fn plugin_error(e: &JsValue) -> String {
        if let Some(s) = e.as_string() {
            if !s.is_empty() {
                return s;
            }
        }
        let (name, message) = js_error_parts(e);
        plugin_error_text(&name, &message)
    }

    /// Whether this page runs inside a Capacitor shell at all: the native
    /// bridge injects `window.Capacitor`. This says nothing about which
    /// plugins are installed; callers must check the plugin they need with
    /// [`capacitor_plugin`].
    fn capacitor_present() -> bool {
        let Some(window) = web_sys::window() else {
            return false;
        };
        let window_value: JsValue = window.into();
        let Ok(capacitor) = Reflect::get(&window_value, &JsValue::from_str("Capacitor")) else {
            return false;
        };
        !capacitor.is_undefined() && !capacitor.is_null()
    }

    /// The Capacitor plugin object named `name`, when this page runs inside a
    /// Capacitor shell that installed that plugin and the named method is a
    /// function, or `None` otherwise. Deliberately strict: a Capacitor shell
    /// without the plugin installed reports `None`, and callers must say
    /// the plugin is missing instead of claiming success through a path
    /// that does nothing in a WebView.
    fn capacitor_plugin(name: &str, method: &str) -> Option<JsValue> {
        let window = web_sys::window()?;
        let window_value: JsValue = window.into();
        let capacitor = Reflect::get(&window_value, &JsValue::from_str("Capacitor")).ok()?;
        if capacitor.is_undefined() || capacitor.is_null() {
            return None;
        }
        let plugins = Reflect::get(&capacitor, &JsValue::from_str("Plugins")).ok()?;
        if plugins.is_undefined() || plugins.is_null() {
            return None;
        }
        let plugin = Reflect::get(&plugins, &JsValue::from_str(name)).ok()?;
        if plugin.is_undefined() || plugin.is_null() {
            return None;
        }
        let func = Reflect::get(&plugin, &JsValue::from_str(method)).ok()?;
        if !func.is_function() {
            return None;
        }
        Some(plugin)
    }

    /// Set one string property on a JS options object. Errors (a non-object,
    /// a frozen object) surface as a message instead of panicking.
    fn set_opt(obj: &js_sys::Object, key: &str, value: &str) -> Result<(), String> {
        Reflect::set(obj.as_ref(), &JsValue::from_str(key), &JsValue::from_str(value))
            .map(|_| ())
            .map_err(|e| plugin_error(&e))
    }

    /// Call `method` on a Capacitor plugin object with one options object,
    /// awaiting the returned promise. Returns the promise's resolved value,
    /// or its rejection value.
    async fn call_plugin(plugin: &JsValue, method: &str, args: &js_sys::Object) -> Result<JsValue, JsValue> {
        let func = Reflect::get(plugin, &JsValue::from_str(method))?;
        if !func.is_function() {
            return Err(JsValue::from_str("plugin method is not a function"));
        }
        let func: js_sys::Function = func.unchecked_into();
        let args_value: &JsValue = args.as_ref();
        let result = func.call1(plugin, args_value)?;
        // `Promise.resolve` adopts an already-a-promise result and wraps
        // anything else, so a plugin that returns a plain value is handled
        // the same way as one that returns a promise.
        let promise = js_sys::Promise::resolve(&result);
        JsFuture::from(promise).await
    }

    /// Write `bytes` with the Capacitor Filesystem plugin and return the
    /// file URI the plugin reports, falling back to `getUri` when the
    /// write result carries none. `directory` is the plugin's Directory
    /// enum name, passed through unchanged ("CACHE" needs no storage
    /// permission; "DOCUMENTS" is the user-visible documents directory).
    /// No permission call is made here: whether the plugin's
    /// `checkPermissions`/`requestPermissions` apply to this directory on
    /// the target device could not be verified from the plugin's official
    /// docs in this change, so no guessed permission call is added; a
    /// denied write surfaces as a `Failed` event with the plugin's reason.
    /// The caller decides what location text the user sees: the raw URI
    /// is returned here and must not be shown as a user-facing path.
    /// Err carries a human-readable message.
    async fn capacitor_write_file(directory: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
        let plugin = capacitor_plugin("Filesystem", "writeFile")
            .ok_or_else(|| "the Filesystem plugin is not installed in this build".to_string())?;
        let clean = safe_file_name(name).to_string();
        let path = format!("lightcraft/{clean}");
        let opts = js_sys::Object::new();
        set_opt(&opts, "path", &path)?;
        set_opt(&opts, "data", &base64_encode(bytes))?;
        set_opt(&opts, "directory", directory)?;
        Reflect::set(opts.as_ref(), &JsValue::from_str("recursive"), &JsValue::from_bool(true))
            .map(|_| ())
            .map_err(|e| plugin_error(&e))?;
        let result = call_plugin(&plugin, "writeFile", &opts).await.map_err(|e| plugin_error(&e))?;
        if let Some(uri) = Reflect::get(&result, &JsValue::from_str("uri")).ok().and_then(|u| u.as_string()) {
            if !uri.is_empty() {
                return Ok(uri);
            }
        }
        let uri_opts = js_sys::Object::new();
        set_opt(&uri_opts, "path", &path)?;
        set_opt(&uri_opts, "directory", directory)?;
        let result = call_plugin(&plugin, "getUri", &uri_opts).await.map_err(|e| plugin_error(&e))?;
        Reflect::get(&result, &JsValue::from_str("uri"))
            .ok()
            .and_then(|u| u.as_string())
            .filter(|uri| !uri.is_empty())
            .ok_or_else(|| "Filesystem write succeeded but returned no file URI".to_string())
    }

    /// Save `bytes` where the user can get at them, answering only from the
    /// real result. In a Capacitor shell the file is written to the
    /// Documents directory with the Filesystem plugin (see
    /// [`capacitor_write_file`]) and the answer reports the human-readable
    /// location `Documents/lightcraft/<name>`, never the raw plugin URI;
    /// in a browser the existing anchor download is kept unchanged.
    async fn save_to_device(shared: Shared, name: String, bytes: Vec<u8>) {
        if capacitor_present() {
            match capacitor_write_file("DOCUMENTS", &name, &bytes).await {
                Ok(_) => shared.push(HostEvent::Saved {
                    name: safe_file_name(&name).to_string(),
                    location: format!("Documents/lightcraft/{}", safe_file_name(&name)),
                }),
                Err(message) => shared.push(HostEvent::Failed { what: "save".into(), message }),
            }
            return;
        }
        download_and_answer(&shared, &name, &bytes);
    }

    /// Share `bytes` through the Capacitor Share plugin: write the file
    /// with Filesystem first, then open the system share sheet with the
    /// file URI. Only the plugin's result answers: a resolved share is
    /// `Saved`, a cancelled share is "Sharing was cancelled.", any other
    /// rejection is `Failed` with the plugin's reason. There is no
    /// download fallback here: the anchor download does not work in a
    /// WebView, so falling back to it would repeat the false-success bug.
    /// A file written before a missing Share plugin is left in the Cache
    /// directory; it is harmless, and the failure says the Share plugin
    /// is missing.
    async fn capacitor_share(shared: Shared, name: String, bytes: Vec<u8>) {
        let clean = safe_file_name(&name).to_string();
        let uri = match capacitor_write_file("CACHE", &name, &bytes).await {
            Ok(uri) => uri,
            Err(message) => {
                shared.push(HostEvent::Failed { what: "share".into(), message });
                return;
            }
        };
        let Some(plugin) = capacitor_plugin("Share", "share") else {
            shared.push(HostEvent::Failed {
                what: "share".into(),
                message: "the Share plugin is not installed in this build".to_string(),
            });
            return;
        };
        let opts = js_sys::Object::new();
        let files = js_sys::Array::of1(&JsValue::from_str(&uri));
        if Reflect::set(opts.as_ref(), &JsValue::from_str("files"), files.as_ref()).is_err() {
            shared.push(HostEvent::Failed { what: "share".into(), message: "could not build the share request".to_string() });
            return;
        }
        if Reflect::set(opts.as_ref(), &JsValue::from_str("dialogTitle"), &JsValue::from_str("Share photo")).is_err() {
            shared.push(HostEvent::Failed { what: "share".into(), message: "could not build the share request".to_string() });
            return;
        }
        match call_plugin(&plugin, "share", &opts).await {
            Ok(_) => shared.push(HostEvent::Saved { name: clean, location: "the share sheet".into() }),
            Err(e) => {
                let (error_name, message) = js_error_parts(&e);
                if is_cancelled(&error_name, &message) {
                    shared.push(HostEvent::Failed { what: "share".into(), message: "Sharing was cancelled.".to_string() });
                } else {
                    shared.push(HostEvent::Failed { what: "share".into(), message: format!("Sharing failed: {}", plugin_error(&e)) });
                }
            }
        }
    }

    /// The MIME type for a download or share, from the file name's extension.
    fn mime_for(name: &str) -> &'static str {
        let clean = safe_file_name(name);
        if clean.ends_with(".jpg") || clean.ends_with(".jpeg") {
            "image/jpeg"
        } else if clean.ends_with(".png") {
            "image/png"
        } else {
            "application/octet-stream"
        }
    }

    /// The `name` property of a browser error, or empty when the error has none.
    fn error_name(e: &JsValue) -> String {
        Reflect::get(e, &"name".into()).ok().and_then(|n| n.as_string()).unwrap_or_default()
    }

    /// Download `bytes` and answer `Saved`, or answer `Failed` with the download error.
    /// Used by `SaveToDevice` and as the share fallback.
    fn download_and_answer(shared: &Shared, name: &str, bytes: &[u8]) {
        let clean = safe_file_name(name).to_string();
        match download(name, bytes) {
            Ok(()) => shared.push(HostEvent::Saved { name: clean.clone(), location: format!("Downloads/{clean}") }),
            Err(message) => shared.push(HostEvent::Failed { what: "save".into(), message }),
        }
    }

    /// Share `bytes` with the system share sheet (Web Share API, files). When the browser
    /// has no share API or cannot share files, the file is downloaded instead and the
    /// answer is `Saved`, so the user still gets the file. A share the user cancels, or
    /// one the browser rejects, answers `Failed` with the real reason. Inside a
    /// Capacitor shell this function is never reached: [`capacitor_share`] runs
    /// instead, because neither `navigator.share` nor the anchor download works
    /// in an Android WebView.
    async fn share_photo(shared: Shared, name: String, bytes: Vec<u8>) {
        if capacitor_present() {
            capacitor_share(shared, name, bytes).await;
            return;
        }
        let Some(window) = web_sys::window() else {
            download_and_answer(&shared, &name, &bytes);
            return;
        };
        let navigator = window.navigator();
        let navigator_value: JsValue = navigator.clone().into();
        let share_fn = Reflect::get(&navigator_value, &"share".into()).ok();
        let share_present = share_fn.as_ref().is_some_and(|f| f.is_function());
        let can_share_fn = Reflect::get(&navigator_value, &"can_share".into()).ok();
        let can_share_present = can_share_fn.as_ref().is_some_and(|f| f.is_function());
        let clean = safe_file_name(&name).to_string();
        let file = match make_share_file(&clean, &bytes) {
            Ok(f) => f,
            Err(message) => {
                shared.push(HostEvent::Failed { what: "share".into(), message });
                return;
            }
        };
        // Build the share payload as a plain JS object: web_sys::ShareData
        // requires --cfg=web_sys_unstable_apis (set nowhere in this repo), and
        // Navigator has no can_share(&ShareData)/share(&ShareData) methods (the
        // overloads are can_share_with_data/share_with_data). Reflect calls
        // avoid the unstable dictionary entirely.
        let data = js_sys::Object::new();
        let files = js_sys::Array::of1(&file);
        if Reflect::set(data.as_ref(), &JsValue::from_str("files"), files.as_ref()).is_err() {
            shared.push(HostEvent::Failed { what: "share".into(), message: "could not build the share request".into() });
            return;
        }
        let can_share = if can_share_present {
            can_share_fn
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .and_then(|f| f.call1(&navigator_value, data.as_ref()).ok())
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        } else {
            false
        };
        if should_fallback_to_download(share_present, can_share) {
            log::info!("lightcraft-mobile: Web Share unavailable for files; downloading instead");
            download_and_answer(&shared, &name, &bytes);
            return;
        }
        let Some(share_fn) = share_fn.and_then(|f| f.dyn_into::<js_sys::Function>().ok()) else {
            shared.push(HostEvent::Failed { what: "share".into(), message: "Sharing is not available in this browser.".into() });
            return;
        };
        let result = match share_fn.call1(&navigator_value, data.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                let message = share_failure_message(&error_name(&e), "Sharing is not available in this browser.");
                shared.push(HostEvent::Failed { what: "share".into(), message });
                return;
            }
        };
        let promise = js_sys::Promise::resolve(&result);
        match JsFuture::from(promise).await {
            Ok(_) => shared.push(HostEvent::Saved { name: clean, location: "the share sheet".into() }),
            Err(e) => {
                let message = share_failure_message(&error_name(&e), "Sharing failed.");
                shared.push(HostEvent::Failed { what: "share".into(), message });
            }
        }
    }

    /// A [`web_sys::File`] holding `bytes`, for the Web Share API.
    fn make_share_file(name: &str, bytes: &[u8]) -> Result<web_sys::File, String> {
        let arr = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let opts = web_sys::FilePropertyBag::new();
        opts.set_type(mime_for(name));
        web_sys::File::new_with_u8_array_sequence_and_options(&arr, name, &opts)
            .map_err(|e| format!("could not prepare {name} for sharing: {e:?}"))
    }

    /// Show the browser's open dialog; the picked file is read asynchronously and answers
    /// [`HostEvent::Picked`]. Cancelling the dialog fires a `cancel` event in current
    /// Chromium-based WebViews and answers `PickCancelled`; in a browser that does not
    /// fire it the app simply waits, as before, and the next pick removes the leftover
    /// input. Every failure path here answers `PickCancelled`, so the app is never left
    /// waiting on a picker that cannot open. The input is appended to the document
    /// body, visually hidden, before it is clicked: a detached input is enough in
    /// desktop Chrome, but in an Android WebView the native file chooser is driven
    /// for inputs in the page, and a detached input may open nothing at all. At most
    /// one pending input exists (`pending_pick`): a new pick removes any previous one
    /// first, and whichever handler runs first (change or cancel) claims the input,
    /// so a pick is never answered twice and hidden inputs never accumulate.
    fn pick_photo(shared: Shared, pending_pick: Rc<RefCell<Option<web_sys::HtmlInputElement>>>) {
        let Some(window) = web_sys::window() else {
            shared.push(HostEvent::PickCancelled);
            return;
        };
        let Some(doc) = window.document() else {
            shared.push(HostEvent::PickCancelled);
            return;
        };
        let Ok(input) = doc.create_element("input").map(|e| e.unchecked_into::<web_sys::HtmlInputElement>()) else {
            shared.push(HostEvent::PickCancelled);
            return;
        };
        input.set_type("file");
        input.set_multiple(false);
        input.set_accept("image/*,.jpg,.jpeg,.png,.webp,.tif,.tiff,.dng,.cr2,.cr3,.nef,.arw,.orf,.rw2,.raf,.pef,.heic,.heif,.avif");
        let Some(body) = doc.body() else {
            shared.push(HostEvent::PickCancelled);
            return;
        };
        // One pending input at most: remove any input a previous pick left
        // behind (its dialog never answered) before creating a new one, so
        // cancelled picks do not leak hidden inputs into the document.
        if let Some(old) = pending_pick.borrow_mut().take() {
            let _ = body.remove_child(old.unchecked_ref());
        }
        let input_node: &web_sys::Node = input.unchecked_ref();
        if input.set_attribute("style", "position:fixed;left:-9999px;top:0;width:1px;height:1px;opacity:0;").is_err()
            || body.append_child(input_node).is_err()
        {
            shared.push(HostEvent::PickCancelled);
            return;
        }
        pending_pick.borrow_mut().replace(input.clone());
        let body_for_handler = body.clone();
        let pending_for_change = pending_pick.clone();
        // Taken before `on_change` moves `shared`.
        let shared_for_cancel = shared.clone();
        let on_change = Closure::<dyn FnMut()>::new(move || {
            // Claim the pending input: whichever handler runs first takes
            // it, and the other finds nothing and stays silent, so a pick
            // is never answered twice.
            let Some(inp) = pending_for_change.borrow_mut().take() else {
                return;
            };
            let _ = body_for_handler.remove_child(inp.unchecked_ref());
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
            } else {
                shared.push(HostEvent::PickCancelled);
            }
        });
        input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
        on_change.forget();
        // The cancel event fires when the dialog is dismissed without a
        // file. It is registered through `EventTarget` (the `cancel`
        // handler property is not a stable web-sys setter on this element),
        // using only the already-enabled `EventTarget` feature. Browsers
        // that do not fire it leave the pick unanswered, as before; the
        // pending input is removed by the next pick. No focus-return
        // fallback is added: a focus event cannot tell the dialog opening
        // from it closing without timer plumbing, so it would risk
        // answering a pick the user has not finished making, and the
        // pending-input guard alone cannot make that safe.
        let pending_for_cancel = pending_pick.clone();
        let body_for_cancel = body.clone();
        let on_cancel = Closure::<dyn FnMut()>::new(move || {
            let Some(inp) = pending_for_cancel.borrow_mut().take() else {
                return;
            };
            let _ = body_for_cancel.remove_child(inp.unchecked_ref());
            shared_for_cancel.push(HostEvent::PickCancelled);
        });
        let _ = input.add_event_listener_with_callback("cancel", on_cancel.as_ref().unchecked_ref());
        on_cancel.forget();
        input.click();
    }

    /// Offer `bytes` as a browser download named `name` (mirrors the desktop web app's
    /// export). Browser-only: in the Capacitor shell, saving goes through the
    /// Filesystem plugin instead (see [`save_to_device`]), because Android
    /// WebView does not honor the anchor `download` attribute.
    fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
        let e = |e: JsValue| format!("download failed: {e:?}");
        let clean = safe_file_name(name);
        let arr = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let opts = web_sys::BlobPropertyBag::new();
        opts.set_type(mime_for(name));
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
    fn share_fallback_only_when_sharing_cannot_work() {
        assert!(!should_fallback_to_download(true, true));
        assert!(should_fallback_to_download(false, true));
        assert!(should_fallback_to_download(true, false));
        assert!(should_fallback_to_download(false, false));
    }

    #[test]
    fn share_failures_carry_a_real_reason() {
        assert_eq!(share_failure_message("AbortError", "Sharing failed."), "Sharing was cancelled.");
        assert_eq!(share_failure_message("NotAllowedError", "Sharing failed."), "Sharing failed (NotAllowedError).");
        assert_eq!(share_failure_message("", "Sharing is not available in this browser."), "Sharing is not available in this browser.");
    }

    #[test]
    fn base64_encode_matches_standard_vectors() {
        assert_eq!(base64_encode(&[]), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(&[0, 255, 254]), "AP/+");
        assert_eq!(base64_encode(&[0, 0, 0]), "AAAA");
    }

    #[test]
    fn cancelled_plugin_errors_are_recognised() {
        assert!(is_cancelled("AbortError", ""));
        assert!(is_cancelled("Error", "Share canceled by user"));
        assert!(is_cancelled("Error", "user cancelled"));
        assert!(!is_cancelled("NotAllowedError", "permission denied"));
        assert_eq!(plugin_error_text("Error", "no space left"), "no space left");
        assert_eq!(plugin_error_text("Error", ""), "Error");
        assert_eq!(plugin_error_text("", ""), "unknown error");
    }

    #[test]
    fn memory_host_round_trip() {
        let mut host = MemoryHost::default();
        host.send(HostRequest::Put { key: "library/index.json".into(), bytes: vec![1, 2] });
        host.send(HostRequest::Get { key: "library/index.json".into() });
        assert_eq!(host.poll(), vec![HostEvent::Loaded { key: "library/index.json".into(), bytes: Some(vec![1, 2]) }]);
    }
}
