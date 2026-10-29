//! Browser entry point: mount the touch-first [`App`](super::ui::App) on
//! `<canvas id="lightcraft_canvas">` (see `apps/lightcraft-mobile/index.html`), with
//! [`WebHost`](super::ui::host::WebHost) as the platform boundary. Compiled only for wasm32;
//! native builds and tests drive [`App`](super::ui::App) with [`MemoryHost`](super::ui::host::MemoryHost).
//!
//! Renders run on the main thread: there are no render workers in this app (unlike the
//! desktop web app), so a large preview render blocks the UI thread. The UI is expected to
//! render a small draft while a slider is dragged and the full preview on release.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::ui::{App, host::WebHost};

fn window() -> Option<web_sys::Window> {
    web_sys::window()
}

fn set_status(text: &str) {
    if let Some(el) = window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id("lightcraft_status")) {
        el.set_text_content(Some(text));
    }
}

/// The eframe app: the mobile [`App`] behind this egui fork's split `logic`/`ui` trait
/// (mirroring `apps/lightcraft-web`'s `WebApp`).
struct MobileApp {
    app: App,
}

impl eframe::App for MobileApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {}

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.frame(ui);
    }
}

/// Start the app. Called once from `index.html` after the WASM module is instantiated.
#[wasm_bindgen]
pub fn start() {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    log::info!("lightcraft-mobile: wasm instantiated");
    wasm_bindgen_futures::spawn_local(async move {
        let Some(canvas) = window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("lightcraft_canvas"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            log::error!("missing <canvas id=\"lightcraft_canvas\">");
            return;
        };
        let runner = eframe::WebRunner::new();
        let r = runner
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(move |cc| {
                    let host = WebHost::new(cc.egui_ctx.clone());
                    Ok(Box::new(MobileApp { app: App::new(Box::new(host)) }))
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
                set_status(&format!("LightCraft Mobile failed to start: {e:?}"));
            }
        }
    });
}
