//! Browser entry point: mount the touch-first [`App`](super::ui::App) on
//! `<canvas id="lightcraft_canvas">` (see `apps/lightcraft-mobile/index.html`), with
//! [`WebHost`](super::ui::host::WebHost) as the platform boundary. Compiled only for wasm32;
//! native builds and tests drive [`App`](super::ui::App) with [`MemoryHost`](super::ui::host::MemoryHost).
//!
//! Renders run on the main thread: there are no render workers in this app (unlike the
//! desktop web app), so a large preview render blocks the UI thread. The UI is expected to
//! render a small draft while a slider is dragged and the full preview on release.

use js_sys::Reflect;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::ui::{App, host::WebHost};

fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

/// Reload the page, through the DOM rather than a typed binding: the
/// `Location` web-sys feature is not enabled for this crate, and the
/// Reflect call needs no new feature.
fn reload_page() {
    let Some(window) = window() else {
        return;
    };
    let window_value: wasm_bindgen::JsValue = window.into();
    let Ok(location) = Reflect::get(&window_value, &wasm_bindgen::JsValue::from_str("location")) else {
        return;
    };
    let Ok(reload) = Reflect::get(&location, &wasm_bindgen::JsValue::from_str("reload")) else {
        return;
    };
    if let Ok(func) = reload.dyn_into::<js_sys::Function>() {
        let _ = func.call0(&location);
    }
}

/// Show a short, plain error in the loading overlay and offer a real
/// Reload button. Raw detail never reaches the screen: callers log it.
/// `set_text_content` replaces any previous children, so a stale button
/// from an earlier error cannot accumulate. The overlay is
/// `pointer-events: none` by default, so the button (and the overlay
/// while an error is shown) opts back in to pointer events.
fn show_error_with_reload(el: &web_sys::Element, short: &str) {
    el.set_text_content(Some(short));
    let _ = el.set_attribute("style", "pointer-events:auto;flex-direction:column;gap:12px;");
    if let Some(doc) = window().and_then(|w| w.document()) {
        if let Ok(button) = doc.create_element("button") {
            button.set_text_content(Some("Reload"));
            let _ = button.set_attribute(
                "style",
                "pointer-events:auto;font:inherit;padding:10px 18px;border-radius:10px;border:1px solid #3a3a3a;background:#262626;color:#eee;",
            );
            let on_click = Closure::<dyn FnMut()>::new(move || reload_page());
            let _ = button.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
            on_click.forget();
            let _ = el.append_child(&button);
        }
    }
}

fn set_status(text: &str) {
    if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_status")) {
        el.set_text_content(Some(text));
    }
}

/// Debug probe (`?debug=1` only): answers the unverified Capacitor/storage
/// questions in one install, without changing normal launches. Everything
/// here is read-only Reflect access through already-enabled features (the
/// same pattern as the ShareData fix in `ui/host.rs`); no new web-sys
/// feature is needed and no storage API is re-requested. The probe runs
/// on the first rendered frame, so its reads are independent browser
/// facts, not the app's backend state: the chosen backend kind and the
/// `persist()` result are logged by `ui/host.rs` / `ui/storage.rs` at
/// open time and are pointed at, not duplicated, because `web.rs` has
/// no accessor for the `Backend` instance owned by `WebHost`.

/// `Reflect::get`, dropping missing/null values. Never panics.
fn reflect_get(obj: &wasm_bindgen::JsValue, key: &str) -> Option<wasm_bindgen::JsValue> {
    Reflect::get(obj, &wasm_bindgen::JsValue::from_str(key))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null())
}

fn bool_word(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "n/a",
    }
}

/// True only when the page URL query contains exactly `debug=1` as one
/// `&`-separated parameter. Default off: normal launches are untouched.
fn debug_enabled() -> bool {
    let Some(window) = window() else {
        return false;
    };
    let window_value: wasm_bindgen::JsValue = window.into();
    let Some(search) = reflect_get(&window_value, "location")
        .and_then(|location| reflect_get(&location, "search"))
        .and_then(|value| value.as_string())
    else {
        return false;
    };
    let query = search.strip_prefix('?').unwrap_or(&search);
    query.split('&').any(|part| part == "debug=1")
}

/// Own enumerable string keys of a JS object, in order.
fn object_key_names(obj: &wasm_bindgen::JsValue) -> Vec<String> {
    let Some(object) = obj.dyn_ref::<js_sys::Object>() else {
        return Vec::new();
    };
    let keys = js_sys::Object::keys(object);
    let mut names = Vec::new();
    for i in 0..keys.length() {
        if let Some(name) = keys.get(i).as_string() {
            names.push(name);
        }
    }
    names
}

/// Call a no-argument method that returns a promise and await it.
/// Returns `None` when the method is missing, is not a function, throws,
/// or rejects.
async fn reflect_call0(obj: &wasm_bindgen::JsValue, method: &str) -> Option<wasm_bindgen::JsValue> {
    let func = reflect_get(obj, method)?.dyn_into::<js_sys::Function>().ok()?;
    let result = func.call0(obj).ok()?;
    wasm_bindgen_futures::JsFuture::from(js_sys::Promise::resolve(&result))
        .await
        .ok()
}

/// Call a one-string-argument method returning a boolean (used for
/// `Capacitor.isPluginAvailable`). Returns `None` when unavailable.
fn reflect_call1_bool(obj: &wasm_bindgen::JsValue, method: &str, arg: &str) -> Option<bool> {
    let func = reflect_get(obj, method)?.dyn_into::<js_sys::Function>().ok()?;
    let result = func.call1(obj, &wasm_bindgen::JsValue::from_str(arg)).ok()?;
    Some(result.is_truthy())
}

async fn collect_debug_probe() -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("lightcraft-mobile debug probe (?debug=1)".to_string());
    let Some(window) = window() else {
        lines.push("window: unavailable".to_string());
        return lines.join("\n");
    };
    let window_value: wasm_bindgen::JsValue = window.into();

    let search = reflect_get(&window_value, "location")
        .and_then(|location| reflect_get(&location, "search"))
        .and_then(|value| value.as_string());
    lines.push(format!(
        "location.search: {}",
        search.as_deref().unwrap_or("n/a")
    ));
    lines.push(format!(
        "isSecureContext: {}",
        bool_word(reflect_get(&window_value, "isSecureContext").and_then(|value| value.as_bool()))
    ));
    let user_agent = reflect_get(&window_value, "navigator")
        .and_then(|navigator| reflect_get(&navigator, "userAgent"))
        .and_then(|value| value.as_string());
    lines.push(format!("userAgent: {}", user_agent.as_deref().unwrap_or("n/a")));

    // StorageManager reads only: the probe never re-requests persistence.
    let storage = reflect_get(&window_value, "navigator")
        .and_then(|navigator| reflect_get(&navigator, "storage"));
    let get_directory = storage.as_ref().and_then(|s| reflect_get(s, "getDirectory"));
    lines.push(format!(
        "navigator.storage.getDirectory: {}",
        match &get_directory {
            None => "missing",
            Some(value) if value.is_function() => "function",
            Some(_) => "present (not a function)",
        }
    ));
    if let Some(storage) = storage {
        let persisted = reflect_call0(&storage, "persisted").await;
        lines.push(format!(
            "navigator.storage.persisted(): {}",
            bool_word(persisted.map(|value| value.is_truthy()))
        ));
        let estimate = reflect_call0(&storage, "estimate").await;
        let (usage, quota) = match &estimate {
            Some(value) => (
                reflect_get(value, "usage").and_then(|v| v.as_f64()),
                reflect_get(value, "quota").and_then(|v| v.as_f64()),
            ),
            None => (None, None),
        };
        let bytes = |v: Option<f64>| match v {
            Some(n) if n.is_finite() && n >= 0.0 => format!("{}", n as u64),
            _ => "n/a".to_string(),
        };
        lines.push(format!(
            "navigator.storage.estimate(): usage={} quota={}",
            bytes(usage),
            bytes(quota)
        ));
    } else {
        lines.push("navigator.storage: missing".to_string());
    }
    lines.push(
        "persist(): not called by the probe; see the console line \
        'lightcraft-mobile: persistent storage ...' logged by storage.rs at open"
            .to_string(),
    );
    lines.push(
        "storage backend kind: not re-readable from web.rs (the Backend \
        lives inside WebHost); see the console line 'lightcraft-mobile: \
        storage backend ...' logged by host.rs at open"
            .to_string(),
    );

    match reflect_get(&window_value, "Capacitor") {
        None => lines.push("window.Capacitor: absent".to_string()),
        Some(capacitor) => {
            match reflect_get(&capacitor, "Plugins") {
                Some(plugins) => {
                    let names = object_key_names(&plugins);
                    lines.push(format!(
                        "Capacitor.Plugins: {}",
                        if names.is_empty() {
                            "(none)".to_string()
                        } else {
                            names.join(", ")
                        }
                    ));
                }
                None => lines.push("Capacitor.Plugins: missing".to_string()),
            }
            lines.push(format!(
                "Capacitor.isPluginAvailable('Filesystem'): {}",
                bool_word(reflect_call1_bool(&capacitor, "isPluginAvailable", "Filesystem"))
            ));
            lines.push(format!(
                "Capacitor.isPluginAvailable('Share'): {}",
                bool_word(reflect_call1_bool(&capacitor, "isPluginAvailable", "Share"))
            ));
            match reflect_get(&capacitor, "PluginHeaders") {
                Some(headers) => {
                    let names = object_key_names(&headers);
                    lines.push(format!(
                        "Capacitor.PluginHeaders: {}",
                        if names.is_empty() {
                            "(none)".to_string()
                        } else {
                            names.join(", ")
                        }
                    ));
                }
                None => lines.push("Capacitor.PluginHeaders: missing".to_string()),
            }
        }
    }

    let content_type = reflect_get(&window_value, "__lightcraftWasmContentType")
        .and_then(|value| value.as_string());
    lines.push(format!(
        "wasm Content-Type: {}",
        content_type.as_deref().unwrap_or("n/a")
    ));
    lines.join("\n")
}

/// Show the probe result in a visible on-page panel that never blocks
/// touches: the panel itself ignores pointer events; only the small
/// Dismiss button opts back in. The same text goes to the console log.
fn show_debug_panel(text: &str) {
    let Some(doc) = window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(panel) = doc.create_element("div") else {
        return;
    };
    panel.set_id("lightcraft_debug");
    let _ = panel.set_attribute(
        "style",
        "position:absolute;top:0;left:0;right:0;max-height:55%;overflow:auto;z-index:10;\
        background:rgba(0,0,0,0.88);color:#eee;font:11px/1.5 ui-monospace,monospace;\
        padding:10px;pointer-events:none;white-space:pre-wrap;word-break:break-word;",
    );
    if let Ok(pre) = doc.create_element("pre") {
        let _ = pre.set_attribute("style", "margin:0;pointer-events:none;");
        pre.set_text_content(Some(text));
        let _ = panel.append_child(&pre);
    }
    if let Ok(button) = doc.create_element("button") {
        button.set_text_content(Some("Dismiss debug"));
        let _ = button.set_attribute(
            "style",
            "pointer-events:auto;margin-top:8px;padding:8px 14px;border-radius:10px;\
            border:1px solid #3a3a3a;background:#262626;color:#eee;font:inherit;",
        );
        let panel_for_button = panel.clone();
        let on_click = Closure::<dyn FnMut()>::new(move || {
            panel_for_button.remove();
        });
        let _ = button.add_event_listener_with_callback("click", on_click.as_ref().unchecked_ref());
        on_click.forget();
        let _ = panel.append_child(&button);
    }
    if let Some(body) = doc.body() {
        let _ = body.append_child(&panel);
    }
}

/// Start the probe once, and only when `?debug=1` is present. Called
/// from the first rendered frame so the app has started.
fn maybe_start_debug_probe() {
    if !debug_enabled() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let text = collect_debug_probe().await;
        log::info!("{text}");
        show_debug_panel(&text);
    });
}

/// Show a short start failure where the user can see it, with a Reload
/// button: replace the loading overlay's text and mirror the short
/// message into the aria-live status element. The raw error detail is
/// the caller's to log, never to show. Without this, a failed start
/// leaves "Loading LightCraft…" on screen forever.
fn show_start_error(short: &str) {
    if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_loading")) {
        show_error_with_reload(&el, short);
    }
    set_status(short);
}

/// Remove the loading overlay once the first frame has rendered. The
/// overlay is `pointer-events: none`, so leaving it up would only cover
/// the canvas with stale "Loading LightCraft…" text.
fn remove_loading_overlay() {
    if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_loading")) {
        el.remove();
    }
}

/// Show a short panic message where the user can see it, with a Reload
/// button: write the short message into the loading overlay (recreating
/// it if the first frame already removed it, so the message is visible
/// rather than hidden) and mirror it into the aria-live status element.
/// The message states the library is kept; the panic detail itself is
/// logged by the hook, never shown, because a panic in this build aborts
/// the module and the app cannot continue in place.
fn show_panic_message(short: &str) {
    if let Some(doc) = window().and_then(|w| w.document()) {
        if let Some(el) = doc.get_element_by_id("lightcraft_loading") {
            show_error_with_reload(&el, short);
        } else if let Ok(created) = doc.create_element("div") {
            created.set_id("lightcraft_loading");
            show_error_with_reload(&created, short);
            if let Some(body) = doc.body() {
                let _ = body.append_child(&created);
            }
        }
    }
    set_status(short);
}

/// Install a panic hook before the runner starts, so a panic anywhere in
/// the module leaves a visible message instead of a frozen canvas. The
/// hook only writes to the DOM; it never panics itself and adds no
/// dependency. Mirrors the desktop app's panic-hook concept in
/// `apps/lightcraft-web/src/safety.rs`, cut down to a message: this
/// build cannot recover in place after a panic.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        // Detail goes to the console log only; the screen shows a short
        // message with a Reload button, never the panic payload.
        log::error!("LightCraft Mobile panic: {info}");
        show_panic_message("LightCraft Mobile stopped unexpectedly. Your library is kept.");
    }));
}

/// The eframe app: the mobile [`App`] behind this egui fork's split `logic`/`ui` trait
/// (mirroring `apps/lightcraft-web`'s `WebApp`).
struct MobileApp {
    app: App,
    /// Whether the first frame has rendered and the loading overlay has
    /// been removed. The overlay is removed on the first frame, not when
    /// `WebRunner::start` resolves, because this fork's `start` may only
    /// resolve when the runner is destroyed.
    loading_removed: bool,
    /// Whether the `?debug=1` probe has been started (once, first frame).
    debug_probe_started: bool,
}

impl eframe::App for MobileApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {}

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.loading_removed {
            self.loading_removed = true;
            remove_loading_overlay();
            if !self.debug_probe_started {
                self.debug_probe_started = true;
                maybe_start_debug_probe();
            }
        }
        self.app.frame(ui);
    }
}

/// Start the app. Called once from `index.html` after the WASM module is instantiated.
#[wasm_bindgen]
pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    install_panic_hook();
    log::info!("lightcraft-mobile: wasm instantiated");
    wasm_bindgen_futures::spawn_local(async move {
        let Some(canvas) = window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("lightcraft_canvas"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("missing <canvas id=\"lightcraft_canvas\">");
            show_start_error("LightCraft Mobile failed to start.");
            return;
        };
        let runner = eframe::WebRunner::new();
        let r = runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |cc| {
                    let host = WebHost::new(cc.egui_ctx.clone());
                    Ok(Box::new(MobileApp { app: App::new(Box::new(host)), loading_removed: false, debug_probe_started: false }))
                }),
            )
            .await;
        match r {
            Ok(()) => {
                if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_loading")) {
                    el.remove();
                }
            }
            Err(e) => {
                log::error!("LightCraft Mobile failed to start: {e:?}");
                show_start_error("LightCraft Mobile failed to start.");
            }
        }
    });
}
