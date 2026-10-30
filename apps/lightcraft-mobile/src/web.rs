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
                "pointer-events:auto;font:inherit;padding:10px 18px;border-radius:8px;border:1px solid #666;background:#2a2a2a;color:#eee;",
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
}

impl eframe::App for MobileApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {}

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !self.loading_removed {
            self.loading_removed = true;
            remove_loading_overlay();
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
                    Ok(Box::new(MobileApp { app: App::new(Box::new(host)), loading_removed: false }))
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
