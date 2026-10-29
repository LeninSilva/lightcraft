//! The interface (egui), shared by the WASM app and native tests.
//!
//! [`App`] is the whole app: the host creates it once with its [`host::Host`] and calls
//! [`App::frame`] every frame. Native tests drive it with [`host::MemoryHost`] and a headless
//! `egui::Context` (`Context::run_ui`).

pub mod host;

use host::Host;

/// The app.
pub struct App {
    host: Box<dyn Host>,
}

impl App {
    pub fn new(host: Box<dyn Host>) -> App {
        App { host }
    }

    /// Call once per frame with the root `Ui` (eframe's `App::ui`, or `Context::run_ui` in tests).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        for _event in self.host.poll() {}
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("LightCraft");
        });
    }
}
