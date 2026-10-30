//! Browser storage for the mobile app, asynchronous: OPFS (Origin Private File System)
//! when the page can write it from the main thread
//! (`FileSystemFileHandle.createWritable`), else IndexedDB. Adapted from
//! `apps/lightcraft-web/src/backend.rs`, cut down to what the host boundary needs
//! (whole-file read/write/remove; no listing, no partial writes, no reset).
//!
//! Keys are `/`-separated paths (OPFS directories; plain IndexedDB keys), as named by
//! [`crate::library`]. The IndexedDB database is deliberately separate from the desktop
//! web app's (`lightcraft-mobile`, not `lightcraft`), so the two apps never share a store.

use std::sync::atomic::{AtomicU8, Ordering};

use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions, IdbDatabase};

const IDB_NAME: &str = "lightcraft-mobile";
const IDB_STORE: &str = "files";

#[derive(Clone)]
pub enum Backend {
    Opfs(FileSystemDirectoryHandle),
    Idb(IdbDatabase),
}

fn err(e: JsValue) -> String {
    e.as_string().or_else(|| e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message()))).unwrap_or_else(|| format!("{e:?}"))
}

/// The `name` of a DOMException / JS error value, when it has one.
fn dom_name(e: &JsValue) -> Option<String> {
    Reflect::get(e, &"name".into()).ok().and_then(|n| n.as_string())
}

/// Like [`err`], but prefixed with the DOMException name (`QuotaExceededError: ...`)
/// so a storage failure says *what* failed, not just its message.
fn err_named(e: &JsValue) -> String {
    match dom_name(e) {
        Some(name) => format!("{name}: {}", err(e.clone())),
        None => err(e.clone()),
    }
}

/// Did an OPFS call fail because the entry doesn't exist (`NotFoundError`)? Any other failure
/// (security, type mismatch, an unreadable or evicted store) is an error, never "absent".
fn not_found(e: &JsValue) -> bool {
    Reflect::get(e, &"name".into()).ok().and_then(|n| n.as_string()).as_deref() == Some("NotFoundError")
}

/// `navigator.storage` in a window or a worker.
fn storage_manager() -> Option<web_sys::StorageManager> {
    let g = js_sys::global();
    let nav = Reflect::get(&g, &"navigator".into()).ok()?;
    let storage = Reflect::get(&nav, &"storage".into()).ok()?;
    (!storage.is_undefined()).then(|| storage.unchecked_into())
}

fn indexed_db() -> Option<web_sys::IdbFactory> {
    let f = Reflect::get(&js_sys::global(), &"indexedDB".into()).ok()?;
    (!f.is_undefined() && !f.is_null()).then(|| f.unchecked_into())
}

/// Can files be written through OPFS writable streams here?
fn opfs_writable() -> bool {
    let g = js_sys::global();
    Reflect::get(&g, &"FileSystemFileHandle".into())
        .ok()
        .filter(|c| !c.is_undefined())
        .and_then(|c| Reflect::get(&c, &"prototype".into()).ok())
        .and_then(|p| Reflect::get(&p, &"createWritable".into()).ok())
        .is_some_and(|f| f.is_function())
}

/// Resolve an IndexedDB request.
async fn idb_req(req: &web_sys::IdbRequest) -> Result<JsValue, String> {
    let r1 = req.clone();
    let p = js_sys::Promise::new(&mut |resolve, reject| {
        let r2 = r1.clone();
        let ok = Closure::once_into_js(move || {
            let _ = resolve.call1(&JsValue::NULL, &r2.result().unwrap_or(JsValue::UNDEFINED));
        });
        let fail = Closure::once_into_js(move |e: JsValue| {
            let _ = reject.call1(&JsValue::NULL, &e);
        });
        r1.set_onsuccess(Some(ok.unchecked_ref()));
        r1.set_onerror(Some(fail.unchecked_ref()));
    });
    JsFuture::from(p).await.map_err(err)
}

/// Wait for an IndexedDB transaction to commit.
async fn idb_done(tx: &web_sys::IdbTransaction) -> Result<(), String> {
    let t1 = tx.clone();
    let p = js_sys::Promise::new(&mut |resolve, reject| {
        let ok = Closure::once_into_js(move || {
            let _ = resolve.call0(&JsValue::NULL);
        });
        let rej = reject.clone();
        let fail = Closure::once_into_js(move |e: JsValue| {
            let _ = rej.call1(&JsValue::NULL, &e);
        });
        let abort = Closure::once_into_js(move |e: JsValue| {
            let _ = reject.call1(&JsValue::NULL, &e);
        });
        t1.set_oncomplete(Some(ok.unchecked_ref()));
        t1.set_onerror(Some(fail.unchecked_ref()));
        t1.set_onabort(Some(abort.unchecked_ref()));
    });
    JsFuture::from(p).await.map(|_| ()).map_err(err)
}

/// Outcome of the persistent-storage request, recorded for the UI to read via
/// [`Backend::persistence_status`]: 0 unknown (not yet answered), 1 granted,
/// 2 not granted (storage stays evictable), 3 unavailable (no storage manager
/// or the request itself failed). Written once by the detached persistence
/// task; read-only afterwards.
static PERSISTENCE: AtomicU8 = AtomicU8::new(0);

/// Ask the browser not to evict our storage under pressure, best effort: a missing
/// storage manager, a rejected request, or a refusal all leave storage usable, they
/// only leave it evictable. Mirrors the desktop web app's `request_persistence`.
/// Records its outcome in [`PERSISTENCE`] and logs it; never returns an error.
async fn request_persistence() {
    let Some(sm) = storage_manager() else {
        PERSISTENCE.store(3, Ordering::Relaxed);
        return;
    };
    let already = match sm.persisted() {
        Ok(p) => JsFuture::from(p).await.ok().is_some_and(|v| v.is_truthy()),
        Err(_) => false,
    };
    if already {
        PERSISTENCE.store(1, Ordering::Relaxed);
        return;
    }
    match sm.persist() {
        Ok(p) => {
            let granted = JsFuture::from(p).await.ok().is_some_and(|v| v.is_truthy());
            PERSISTENCE.store(if granted { 1 } else { 2 }, Ordering::Relaxed);
            log::info!("lightcraft-mobile: persistent storage {}", if granted { "granted" } else { "not granted" });
        }
        Err(_) => {
            PERSISTENCE.store(3, Ordering::Relaxed);
        }
    }
}

impl Backend {
    /// The persistent-storage request outcome, for the UI: "unknown" (not yet
    /// answered), "granted", "not granted" (storage stays evictable), or
    /// "unavailable". The request runs detached from opening (see [`Backend::open`]),
    /// so this starts as "unknown" and settles later; it is a status, never a gate.
    pub fn persistence_status() -> &'static str {
        match PERSISTENCE.load(Ordering::Relaxed) {
            1 => "granted",
            2 => "not granted",
            3 => "unavailable",
            _ => "unknown",
        }
    }

    /// OPFS if writable from here, else IndexedDB. The persistent-storage request
    /// runs detached in the background and never blocks opening: a hung
    /// `persist()` promise must not stall startup into the host's open watchdog.
    /// A failed `get_directory()` is an error, not a silent fallback: when OPFS
    /// was expected to work, falling back would present a different, empty
    /// IndexedDB library and strand saves in the other store. IndexedDB is
    /// used only when OPFS is affirmatively unsupported here (no writable
    /// OPFS API, or no storage manager to ask for a directory).
    pub async fn open() -> Result<Backend, String> {
        // Detached: `Backend::open` is always awaited inside a spawned task
        // (the host's storage worker), which is the context `spawn_local`
        // needs. The task records its outcome in `PERSISTENCE`.
        wasm_bindgen_futures::spawn_local(async {
            request_persistence().await;
        });
        if opfs_writable() {
            if let Some(sm) = storage_manager() {
                return match JsFuture::from(sm.get_directory()).await {
                    Ok(root) => Ok(Backend::Opfs(root.unchecked_into())),
                    Err(e) => Err(format!("OPFS unavailable: {}", err_named(&e))),
                };
            }
        }
        Self::open_idb().await
    }

    /// Resolve an IndexedDB *open* request: success, a distinct message when the
    /// stored database is from a newer version, an `onblocked` log while
    /// another connection holds the database, and a 10-second timeout so a
    /// stalled open can never hang startup forever. The timeout only rejects
    /// the still-pending promise; a later success is a harmless no-op.
    async fn idb_open(req: &web_sys::IdbOpenDbRequest) -> Result<JsValue, String> {
        let r1 = req.clone();
        let p = js_sys::Promise::new(&mut |resolve, reject| {
            let r2 = r1.clone();
            let ok = Closure::once_into_js(move || {
                let _ = resolve.call1(&JsValue::NULL, &r2.result().unwrap_or(JsValue::UNDEFINED));
            });
            let r3 = r1.clone();
            let rej_fail = reject.clone();
            let fail = Closure::once_into_js(move |_event: JsValue| {
                // `IDBRequest.error` has no typed binding in this web-sys
                // version, so read the property through Reflect, the same
                // way `dom_name` reads `name`.
                let err_value = Reflect::get(r3.as_ref(), &JsValue::from_str("error"))
                    .ok()
                    .filter(|v| !v.is_null() && !v.is_undefined());
                let reason = match err_value.as_ref() {
                    Some(v) if dom_name(v).as_deref() == Some("VersionError") => {
                        "stored IndexedDB database is from a newer version (VersionError); close other copies of the app and retry".to_string()
                    }
                    Some(v) => err_named(v),
                    None => "IndexedDB open failed".to_string(),
                };
                let _ = rej_fail.call1(&JsValue::NULL, &JsValue::from_str(&reason));
            });
            let blocked = Closure::once_into_js(move || {
                log::warn!("lightcraft-mobile: IndexedDB open blocked by another connection; waiting for it to close");
            });
            r1.set_onsuccess(Some(ok.unchecked_ref()));
            r1.set_onerror(Some(fail.unchecked_ref()));
            r1.set_onblocked(Some(blocked.unchecked_ref()));
            if let Some(window) = web_sys::window() {
                let rej_timeout = reject.clone();
                let timeout = Closure::once_into_js(move || {
                    let _ = rej_timeout.call1(&JsValue::NULL, &JsValue::from_str("IndexedDB open timed out after 10 seconds"));
                });
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(timeout.unchecked_ref(), 10_000);
            }
        });
        JsFuture::from(p).await.map_err(err)
    }

    async fn open_idb() -> Result<Backend, String> {
        let f = indexed_db().ok_or("no IndexedDB")?;
        let req = f.open_with_u32(IDB_NAME, 1).map_err(err)?;
        let upgrade = Closure::<dyn FnMut(web_sys::IdbVersionChangeEvent)>::new(move |e: web_sys::IdbVersionChangeEvent| {
            if let Some(db) = e.target().and_then(|t| t.dyn_into::<web_sys::IdbOpenDbRequest>().ok()).and_then(|r| r.result().ok()) {
                let db: IdbDatabase = db.unchecked_into();
                if !db.object_store_names().contains(IDB_STORE) {
                    let _ = db.create_object_store(IDB_STORE);
                }
            }
        });
        req.set_onupgradeneeded(Some(upgrade.as_ref().unchecked_ref()));
        let db_value = Self::idb_open(&req).await?;
        drop(upgrade);
        let db: IdbDatabase = db_value.unchecked_into();
        // If another context upgrades the database, this connection must close
        // instead of holding the old version open and blocking the upgrade.
        // One closure per open, kept alive for the life of the connection.
        let db_for_close = db.clone();
        let on_version_change = Closure::<dyn FnMut(web_sys::IdbVersionChangeEvent)>::new(move |_e: web_sys::IdbVersionChangeEvent| {
            log::warn!("lightcraft-mobile: IndexedDB version change from another context; closing this connection");
            db_for_close.close();
        });
        db.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
        on_version_change.forget();
        Ok(Backend::Idb(db))
    }

    /// "opfs" or "idb".
    pub fn kind(&self) -> &'static str {
        match self {
            Backend::Opfs(_) => "opfs",
            Backend::Idb(_) => "idb",
        }
    }

    /// The directory holding `path`'s last component (created when `create`).
    async fn parent(root: &FileSystemDirectoryHandle, path: &str, create: bool) -> Result<Option<(FileSystemDirectoryHandle, String)>, String> {
        let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let name = parts.pop().ok_or("empty path")?.to_string();
        let mut dir = root.clone();
        for p in parts {
            let opts = FileSystemGetDirectoryOptions::new();
            opts.set_create(create);
            match JsFuture::from(dir.get_directory_handle_with_options(p, &opts)).await {
                Ok(d) => dir = d.unchecked_into(),
                Err(e) if !create && not_found(&e) => return Ok(None),
                Err(e) => return Err(err(e)),
            }
        }
        Ok(Some((dir, name)))
    }

    async fn opfs_file(root: &FileSystemDirectoryHandle, path: &str, create: bool) -> Result<Option<FileSystemFileHandle>, String> {
        let Some((dir, name)) = Self::parent(root, path, create).await? else { return Ok(None) };
        let opts = FileSystemGetFileOptions::new();
        opts.set_create(create);
        match JsFuture::from(dir.get_file_handle_with_options(&name, &opts)).await {
            Ok(h) => Ok(Some(h.unchecked_into())),
            Err(e) if !create && not_found(&e) => Ok(None),
            Err(e) => Err(err(e)),
        }
    }

    fn idb_store(db: &IdbDatabase, write: bool) -> Result<(web_sys::IdbTransaction, web_sys::IdbObjectStore), String> {
        let mode = if write { web_sys::IdbTransactionMode::Readwrite } else { web_sys::IdbTransactionMode::Readonly };
        let tx = db.transaction_with_str_and_mode(IDB_STORE, mode).map_err(err)?;
        let store = tx.object_store(IDB_STORE).map_err(err)?;
        Ok((tx, store))
    }

    /// A whole file, or `None` if it doesn't exist.
    pub async fn read(&self, path: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Backend::Opfs(root) => {
                let Some(h) = Self::opfs_file(root, path, false).await? else { return Ok(None) };
                let file: web_sys::File = JsFuture::from(h.get_file()).await.map_err(err)?.unchecked_into();
                let buf = JsFuture::from(file.array_buffer()).await.map_err(err)?;
                Ok(Some(Uint8Array::new(&buf).to_vec()))
            }
            Backend::Idb(db) => {
                let (_tx, store) = Self::idb_store(db, false)?;
                let v = idb_req(&store.get(&path.into()).map_err(err)?).await?;
                if v.is_undefined() || v.is_null() {
                    return Ok(None);
                }
                // Only byte arrays are ever written here. A stored value of
                // any other type (tampered store, another writer) must be an
                // error, never a `Uint8Array` construction that throws inside
                // the async task and hangs the request with no answer.
                if v.dyn_ref::<Uint8Array>().is_none() && v.dyn_ref::<js_sys::ArrayBuffer>().is_none() {
                    return Err("stored value is not a byte array".to_string());
                }
                Ok(Some(Uint8Array::new(&v).to_vec()))
            }
        }
    }

    /// Replace a file atomically (OPFS: the writable's swap file is committed on close).
    pub async fn write(&self, path: &str, data: &[u8]) -> Result<(), String> {
        match self {
            Backend::Opfs(root) => {
                let h = Self::opfs_file(root, path, true).await?.ok_or("can't create file")?;
                let opts = web_sys::FileSystemCreateWritableOptions::new();
                // Replace, never append to or keep a longer previous file:
                // without this an over-write that shrinks the file would leave
                // stale trailing bytes behind the new content.
                opts.set_keep_existing_data(false);
                let w: web_sys::FileSystemWritableFileStream =
                    JsFuture::from(h.create_writable_with_options(&opts)).await.map_err(|e| err_named(&e))?.unchecked_into();
                let result = async {
                    JsFuture::from(w.seek_with_f64(0.0).map_err(|e| err_named(&e))?).await.map_err(|e| err_named(&e))?;
                    JsFuture::from(w.truncate_with_f64(0.0).map_err(|e| err_named(&e))?).await.map_err(|e| err_named(&e))?;
                    JsFuture::from(w.write_with_js_u8_array(&Uint8Array::from(data)).map_err(|e| err_named(&e))?).await.map_err(|e| err_named(&e))?;
                    JsFuture::from(w.close()).await.map_err(|e| err_named(&e))?;
                    Ok::<(), String>(())
                }
                .await;
                match result {
                    Ok(()) => Ok(()),
                    Err(e) => match JsFuture::from(w.abort()).await {
                        // The swap file is discarded; the previous file stays intact.
                        Ok(_) => Err(e),
                        Err(abort_err) => Err(format!("{e}; abort failed: {}", err_named(&abort_err))),
                    },
                }
            }
            Backend::Idb(db) => {
                let (tx, store) = Self::idb_store(db, true)?;
                store.put_with_key(&Uint8Array::from(data), &path.into()).map_err(err)?;
                idb_done(&tx).await
            }
        }
    }

    pub async fn remove(&self, path: &str) -> Result<(), String> {
        match self {
            Backend::Opfs(root) => {
                let Some((dir, name)) = Self::parent(root, path, false).await? else { return Ok(()) };
                match JsFuture::from(dir.remove_entry(&name)).await {
                    Ok(_) => Ok(()),
                    Err(e) if not_found(&e) => Ok(()),
                    Err(e) => Err(err(e)),
                }
            }
            Backend::Idb(db) => {
                let (tx, store) = Self::idb_store(db, true)?;
                store.delete(&path.into()).map_err(err)?;
                idb_done(&tx).await
            }
        }
    }
}
