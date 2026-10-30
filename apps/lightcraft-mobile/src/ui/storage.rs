//! Browser storage for the mobile app, asynchronous: OPFS (Origin Private File System)
//! when the page can write it from the main thread
//! (`FileSystemFileHandle.createWritable`), else IndexedDB. Adapted from
//! `apps/lightcraft-web/src/backend.rs`, cut down to what the host boundary needs
//! (whole-file read/write/remove; no listing, no partial writes, no reset).
//!
//! Keys are `/`-separated paths (OPFS directories; plain IndexedDB keys), as named by
//! [`crate::library`]. The IndexedDB database is deliberately separate from the desktop
//! web app's (`lightcraft-mobile`, not `lightcraft`), so the two apps never share a store.

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

/// Ask the browser not to evict our storage under pressure, best effort: a missing
/// storage manager, a rejected request, or a refusal all leave storage usable, they
/// only leave it evictable. Mirrors the desktop web app's `request_persistence`.
async fn request_persistence() {
    let Some(sm) = storage_manager() else { return };
    let already = match sm.persisted() {
        Ok(p) => JsFuture::from(p).await.ok().is_some_and(|v| v.is_truthy()),
        Err(_) => false,
    };
    if already {
        return;
    }
    match sm.persist() {
        Ok(p) => {
            let granted = JsFuture::from(p).await.ok().is_some_and(|v| v.is_truthy());
            log::info!("lightcraft-mobile: persistent storage {}", if granted { "granted" } else { "not granted" });
        }
        Err(_) => {}
    }
}

impl Backend {
    /// OPFS if writable from here, else IndexedDB. Persistence is requested first, so
    /// the library and saved versions are less likely to be evicted under storage
    /// pressure; the request never fails opening.
    pub async fn open() -> Result<Backend, String> {
        request_persistence().await;
        if opfs_writable()
            && let Some(sm) = storage_manager()
        {
            match JsFuture::from(sm.get_directory()).await {
                Ok(root) => return Ok(Backend::Opfs(root.unchecked_into())),
                Err(e) => log::warn!("OPFS unavailable ({}); using IndexedDB", err(e)),
            }
        }
        Self::open_idb().await
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
        let db = idb_req(&req).await?;
        drop(upgrade);
        Ok(Backend::Idb(db.unchecked_into()))
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
                let w: web_sys::FileSystemWritableFileStream =
                    JsFuture::from(h.create_writable_with_options(&opts)).await.map_err(err)?.unchecked_into();
                let res = async { JsFuture::from(w.write_with_js_u8_array(&Uint8Array::from(data)).map_err(err)?).await.map_err(err) }.await;
                match res {
                    Ok(()) => JsFuture::from(w.close()).await.map(|_| ()).map_err(err),
                    Err(e) => {
                        let _ = JsFuture::from(w.abort()).await;
                        Err(e)
                    }
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
