//! The touch interface: a minimal, photographer friendly editor.
//!
//! [`App`] is the whole app: the host creates it once with its [`host::Host`] and calls
//! [`App::frame`] every frame. Native tests drive it with [`host::MemoryHost`] and a headless
//! `egui::Context` (`Context::run_ui`).
//!
//! What this interface does, and what it deliberately does not do:
//!
//! - Two screens: a library grid and one editor. The editor has a large photo area, a
//!   bottom tab bar (Auto, Looks, Tune, Focus, Crop) and large touch targets (44 points
//!   or taller). Every button either performs a real action or is disabled with a visible
//!   reason next to it. There are no placeholder buttons.
//! - While a slider is dragged the preview renders as a fast draft at a small size; when
//!   the drag ends the next frame renders the full preview. The render itself is
//!   [`photo::Photo::render`]; this file only chooses the size and quality.
//! - Pinch zoom and double tap zoom are out of scope: there is no button for them, and the
//!   Crop panel says so in plain text. Framing is done with crop, rotation and straighten.
//! - The Auto panel shows exactly what [`advisor::advise`] returns: a summary, tags and
//!   one tap fixes. At the current HEAD the advisor is a stub that returns a summary and
//!   no tips, so the panel says that plainly instead of inventing guidance. The same
//!   applies to [`analysis::analyze`] (a stub at this HEAD) and [`looks::LOOKS`] (one
//!   look, "Clear", at this HEAD). This file codes against those public types only and
//!   changes none of their signatures, so the parallel work on them can land underneath
//!   it without edits here.
//! - The library follows [`library::Library`]: kept photos stay, the most recent
//!   unkept photos stay up to [`library::KEEP_RECENT`], and every saved version stays
//!   until deleted. Thumbnails are stored under the keys [`library`] names and are
//!   decoded only when a frame can upload them, so texture creation never happens
//!   inside host event handling, which has no `egui::Context`.

pub mod host;
#[cfg(target_arch = "wasm32")]
pub mod storage;

use std::collections::HashMap;
use std::sync::Arc;

use egui::{ColorImage, TextureHandle, TextureOptions};
use host::{Host, HostEvent, HostRequest};
use lightcraft_color::transfer::encode_srgb8;
use lightcraft_color::{REC2020, SRGB};
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::Quality;

use crate::advisor::{Advice, Tip};
use crate::analysis::Analysis;
use crate::library::{
    INDEX_BACKUP_KEY, INDEX_KEY, KEEP_RECENT, Library, export_file_name, original_key, thumb_key,
    version_file_name, version_key, version_thumb_key,
};
use crate::looks::{LOOKS, look};
use crate::photo::{JPEG_QUALITY, Photo, encode_jpeg, export_jpeg, export_size};
use crate::recipe::{Aspect, History, Recipe, TuneGroup, TUNE_CONTROLS, compose};

/// Minimum touch target height, in points. Every button and slider row is at
/// least this tall.
const MIN_TOUCH: f32 = 44.0;
/// Height of the bottom tab bar, in points.
const TAB_BAR_HEIGHT: f32 = 48.0;
/// Long edge, in pixels, of the full preview and of a draft preview while dragging.
const FULL_PREVIEW_EDGE: usize = 1024;
const DRAFT_PREVIEW_EDGE: usize = 480;
/// Long edge, in pixels, of a look thumbnail and a library thumbnail render.
const THUMB_EDGE: usize = 256;
const LOOK_THUMB_EDGE: usize = 160;

/// What one [`App::float_slider`] row shows and how it behaves, bundled so
/// the helper stays under the workspace `too_many_arguments` lint.
struct SliderSpec<'a> {
    label: &'a str,
    min: f64,
    max: f64,
    step: f64,
    key: &'a str,
    reset_to: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Screen {
    #[default]
    Library,
    Editor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Auto,
    Looks,
    Tune,
    Focus,
    Crop,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LibraryTab {
    #[default]
    Photos,
    Kept,
    Versions,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingOpen {
    Photo(u64),
    Version(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemovalTarget {
    Photo(u64),
    Version(u64),
}

/// A cached preview texture and the key that describes exactly what it shows.
struct Preview {
    key: String,
    tex: TextureHandle,
    size: [usize; 2],
}

/// The photo open in the editor, with the bytes it was opened from (needed to
/// render a full resolution version later) and the analysis and advice for it.
struct OpenPhoto {
    entry_id: Option<u64>,
    version_id: Option<u64>,
    /// The library photo a saved version came from, or the entry itself.
    source_photo_id: u64,
    name: String,
    bytes: Vec<u8>,
    photo: Photo,
    analysis: Analysis,
    advice: Advice,
}

/// Owned snapshot of a library photo, so drawing a grid never holds a borrow
/// of the library while buttons act on it.
#[derive(Clone, Debug)]
struct EntryView {
    id: u64,
    name: String,
    width: u32,
    height: u32,
    kept: bool,
}

#[derive(Clone, Debug)]
struct VersionView {
    id: u64,
    name: String,
    width: u32,
    height: u32,
}

/// An action a library tile asked for, applied after the grid is drawn.
#[derive(Clone, Copy, Debug)]
enum GridAction {
    OpenPhoto(u64),
    ToggleKeep(u64),
    RemovePhoto(u64),
    OpenVersion(u64),
    ExportVersion(u64),
    DeleteVersion(u64),
}

/// The app.
pub struct App {
    host: Box<dyn Host>,
    screen: Screen,
    library_tab: LibraryTab,
    tab: Tab,
    tune_group: TuneGroup,
    library: Library,
    library_loaded: bool,
    current: Option<OpenPhoto>,
    recipe: Recipe,
    history: History,
    status: String,
    status_error: bool,
    /// A slider was being dragged on the previous frame.
    dragging: bool,
    /// The history key of the gesture in progress, if any.
    gesture_key: Option<String>,
    /// The "hold for original" button was held on the previous frame.
    holding_compare: bool,
    /// Compare is latched on by a quick tap of the Compare button; a
    /// press-and-hold compares only while held and never changes this.
    compare_latched: bool,
    /// When the current Compare press started, in host milliseconds, so a
    /// quick tap can be told apart from a hold. `None` when not pressed.
    compare_press_ms: Option<f64>,
    /// Thumbnail regenerations waiting on a source blob: source blob key
    /// to the thumbnail key and recipe to render it with. A missing or
    /// corrupt stored thumbnail is regenerated from the original or
    /// version blob instead of showing "No preview" forever.
    thumb_regen: HashMap<String, (String, Recipe)>,
    preview: Option<Preview>,
    /// Thumbnail textures by storage key, with their pixel size.
    thumbs: HashMap<String, (TextureHandle, [usize; 2])>,
    /// Look thumbnail textures by look id, with their pixel size.
    look_thumbs: HashMap<String, (TextureHandle, [usize; 2])>,
    /// Which photo the look thumbnails belong to (entry/version identity key).
    look_thumbs_for: String,
    /// Thumbnail blobs waiting for a frame with a context to upload them.
    pending_thumbs: Vec<(String, Vec<u8>)>,
    pending_open: Option<PendingOpen>,
    pending_export_version: Option<u64>,
    armed_remove: Option<RemovalTarget>,
    /// The stored index (and its backup) could not be read: index writes are
    /// blocked until a good index loads, so a corrupt index is never saved
    /// over. Cleared only by `adopt_library` after a successful parse.
    index_write_blocked: bool,
    /// A stored index was successfully parsed and adopted (or no index
    /// was stored yet, which adopts an empty library). Until this is set,
    /// a failed index load must start backup recovery, never silently
    /// show an empty library.
    good_index_loaded: bool,
    /// The last index load failed and no good index was adopted: the
    /// library screen shows a persistent error with a Retry button.
    index_load_failed: bool,
    /// A Put of the index failed: the library screen shows a persistent
    /// error with a Retry button, and automatic index flushes pause
    /// until the user retries.
    index_save_failed: bool,
    /// A backup index load is in flight after a corrupt main index.
    awaiting_index_backup: bool,
    /// The library index needs writing (a recipe changed); flushed at the end
    /// of a frame when no slider is being dragged, so a drag writes once.
    dirty_library: bool,
}

impl App {
    pub fn new(host: Box<dyn Host>) -> App {
        let mut app = App {
            host,
            screen: Screen::Library,
            library_tab: LibraryTab::Photos,
            tab: Tab::Auto,
            tune_group: TuneGroup::Light,
            library: Library::default(),
            library_loaded: false,
            current: None,
            recipe: Recipe::default(),
            history: History::default(),
            status: String::new(),
            status_error: false,
            dragging: false,
            gesture_key: None,
            holding_compare: false,
            compare_latched: false,
            compare_press_ms: None,
            thumb_regen: HashMap::new(),
            preview: None,
            thumbs: HashMap::new(),
            look_thumbs: HashMap::new(),
            look_thumbs_for: String::new(),
            pending_thumbs: Vec::new(),
            pending_open: None,
            pending_export_version: None,
            armed_remove: None,
            index_write_blocked: false,
            awaiting_index_backup: false,
            good_index_loaded: false,
            index_load_failed: false,
            index_save_failed: false,
            dirty_library: false,
        };
        app.host.send(HostRequest::Get { key: INDEX_KEY.to_string() });
        app
    }

    /// Call once per frame with the root `Ui` (eframe's `App::ui`, or `Context::run_ui` in tests).
    pub fn frame(&mut self, ui: &mut egui::Ui) {
        for event in self.host.poll() {
            self.handle_event(event);
        }
        let ctx = ui.ctx().clone();
        self.upload_pending_thumbs(&ctx);
        let (any_dragged, holding) = egui::CentralPanel::default()
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                match self.screen {
                    Screen::Library => {
                        self.library_ui(ui);
                        (false, false)
                    }
                    Screen::Editor => {
                        if self.current.is_none() {
                            self.screen = Screen::Library;
                            self.library_ui(ui);
                            (false, false)
                        } else {
                            self.editor_ui(ui)
                        }
                    }
                }
            })
            .inner;
        if self.dragging && !any_dragged {
            self.history.seal();
            self.gesture_key = None;
        }
        self.dragging = any_dragged;
        self.holding_compare = holding;
        if self.dirty_library && !self.dragging && !self.index_save_failed {
            // Keep the pending write when it could not be sent (library
            // not loaded, backup recovery in flight, or writes blocked):
            // clearing it would drop the edit that still needs saving.
            if self.save_library() {
                self.dirty_library = false;
            }
        }
        if self.dragging {
            ui.ctx().request_repaint();
        }
    }

    fn set_status(&mut self, message: String, error: bool) {
        self.status = message;
        self.status_error = error;
    }

    /// Write the library index (and its backup copy). Returns false, having
    /// sent nothing, when the library has not loaded yet, when backup
    /// recovery is in flight, or when writes are blocked after an
    /// unreadable index: in all three cases the in-memory library is
    /// incomplete or unreconciled, and persisting it could overwrite the
    /// stored index with an empty or partial one. The caller keeps its
    /// pending write (see `frame`) so it is retried once a good index
    /// loads.
    fn save_library(&mut self) -> bool {
        if self.index_write_blocked || !self.library_loaded || self.awaiting_index_backup {
            return false;
        }
        let bytes = self.library.to_json().into_bytes();
        self.host.send(HostRequest::Put { key: INDEX_KEY.to_string(), bytes: bytes.clone() });
        self.host.send(HostRequest::Put { key: INDEX_BACKUP_KEY.to_string(), bytes });
        true
    }

    fn handle_event(&mut self, event: HostEvent) {
        match event {
            HostEvent::Picked { name, bytes } => {
                self.open_picked(name, bytes);
            }
            HostEvent::PickCancelled => {
                self.set_status("No photo picked.".to_string(), false);
            }
            HostEvent::Loaded { key, bytes } => {
                self.handle_loaded(key, bytes);
            }
            HostEvent::Saved { name, location } => {
                // A share success arrives as Saved with the share sheet as
                // its location; it is not a save, so it must not say so.
                if location == "the share sheet" {
                    self.set_status(format!("Shared {name}."), false);
                } else {
                    self.set_status(format!("Saved {name} to {location}."), false);
                }
            }
            HostEvent::Failed { what, message } => {
                if what == "storage" {
                    self.note_storage_failure(&message);
                }
                self.set_status(format!("Could not {what}: {message}"), true);
            }
        }
    }

    /// A storage failure ends the loads it can no longer answer. A failed
    /// index Put keeps the pending write (`dirty_library`) and raises a
    /// persistent error with Retry, instead of the write being dropped.
    /// A failed index load starts backup recovery (see
    /// `start_corrupt_index_recovery`) and blocks index writes until a
    /// good index is adopted. Storage failures name the key they were
    /// loading as `loading {key}: ...` or storing as `storing {key}: ...`;
    /// a backend that never opened names no key, and then no load answer
    /// will ever arrive. Delete failures leave pending loads alone.
    fn note_storage_failure(&mut self, message: &str) {
        if let Some(rest) = message.strip_prefix("storing ") {
            let key = rest.split(':').next().unwrap_or("");
            if key == INDEX_KEY || key == INDEX_BACKUP_KEY {
                self.dirty_library = true;
                self.index_save_failed = true;
            }
            return;
        }
        if message.starts_with("deleting ") {
            return;
        }
        let load_key = message
            .strip_prefix("loading ")
            .and_then(|rest| rest.split(':').next());
        match load_key {
            Some(key) => {
                if self.pending_open_wants(key) {
                    self.pending_open = None;
                }
                if self
                    .pending_export_version
                    .is_some_and(|id| version_key(id) == key)
                {
                    self.pending_export_version = None;
                }
                if key == INDEX_KEY {
                    // No good index adopted yet: the stored index could
                    // not be read, so try the backup copy and block
                    // writes until one of them parses.
                    if !self.good_index_loaded && !self.awaiting_index_backup {
                        self.index_load_failed = true;
                        self.start_corrupt_index_recovery(
                            "The saved library index could not be loaded.",
                        );
                    }
                } else if key == INDEX_BACKUP_KEY && self.awaiting_index_backup {
                    self.note_index_load_failed();
                }
            }
            None => {
                // A generic backend failure: the pending loads have no
                // answer coming, and an index load in flight is over.
                self.pending_open = None;
                self.pending_export_version = None;
                if !self.library_loaded {
                    self.note_index_load_failed();
                }
            }
        }
    }

    /// The pending open, if any, is waiting on this storage key.
    fn pending_open_wants(&self, key: &str) -> bool {
        match &self.pending_open {
            Some(PendingOpen::Photo(id)) => original_key(*id) == key,
            Some(PendingOpen::Version(id)) => version_key(*id) == key,
            None => false,
        }
    }

    /// An index load failed: show the library (empty) instead of
    /// "Loading your library." forever, and, when no good index was
    /// adopted, block index writes and raise the persistent load error
    /// so the unreadable stored index is never overwritten with an empty
    /// one. The block is cleared only by `adopt_library` after a
    /// successful parse.
    fn note_index_load_failed(&mut self) {
        self.library_loaded = true;
        if !self.good_index_loaded {
            self.index_write_blocked = true;
            self.index_load_failed = true;
        }
        self.awaiting_index_backup = false;
    }

    /// Retry after a failed index load or a failed index save, from the
    /// Retry button on the library screen. When no good index was ever
    /// adopted, the retry re-sends the Get for the main index (backup
    /// recovery runs again if that load also fails). Otherwise it
    /// re-attempts the pending index write.
    fn retry_storage(&mut self) {
        if !self.good_index_loaded {
            self.index_load_failed = false;
            self.index_save_failed = false;
            self.awaiting_index_backup = false;
            self.library_loaded = false;
            self.host.send(HostRequest::Get { key: INDEX_KEY.to_string() });
            self.set_status("Trying to load the saved library again.".to_string(), false);
        } else {
            self.index_save_failed = false;
            if self.save_library() {
                self.dirty_library = false;
                self.set_status("The library index was saved.".to_string(), false);
            } else {
                self.index_save_failed = true;
                self.set_status("The library index still could not be saved.".to_string(), true);
            }
        }
    }

    fn handle_loaded(&mut self, key: String, bytes: Option<Vec<u8>>) {
        if key == INDEX_BACKUP_KEY {
            // Only a backup load requested after a corrupt main index counts;
            // a stale or duplicate backup answer is ignored.
            if !self.awaiting_index_backup {
                return;
            }
            self.awaiting_index_backup = false;
            let good = match bytes {
                Some(b) => String::from_utf8(b).ok().and_then(|t| Library::from_json_strict(&t).ok()),
                None => None,
            };
            match good {
                Some(lib) => {
                    self.adopt_library(lib);
                    // Repair: write the adopted library back as the main
                    // index, so the next launch does not need the backup
                    // again. `adopt_library` cleared the write block.
                    self.save_library();
                    self.set_status(
                        "The saved library index could not be read, so the backup copy was used instead.".to_string(),
                        false,
                    );
                }
                None => self.finish_corrupt_index(),
            }
            return;
        }
        if key == INDEX_KEY {
            match bytes {
                None => {
                    // First run: no stored index yet. Adopting the empty
                    // library is the successful parse that clears the
                    // write block, so a later failure cannot leave
                    // writes blocked forever.
                    self.adopt_library(Library::default());
                }
                Some(b) => match String::from_utf8(b) {
                    Ok(text) => match Library::from_json_strict(&text) {
                        Ok(lib) => self.adopt_library(lib),
                        Err(e) => self.start_corrupt_index_recovery(&format!(
                            "The saved library index could not be read ({e})."
                        )),
                    },
                    Err(_) => self.start_corrupt_index_recovery(
                        "The saved library index could not be read (it is not valid text).",
                    ),
                },
            }
            return;
        }
        // A pending open, export or share owns this blob; thumbnails do not.
        if let Some(pending) = self.pending_open.clone() {
            let wanted = match pending {
                PendingOpen::Photo(id) => original_key(id),
                PendingOpen::Version(id) => version_key(id),
            };
            if key == wanted {
                self.pending_open = None;
                self.open_loaded(pending, bytes);
                return;
            }
        }
        if let Some(id) = self.pending_export_version {
            if key == version_key(id) {
                self.pending_export_version = None;
                match bytes {
                    Some(b) => {
                        let name = self
                            .library
                            .version(id)
                            .map(|v| v.name.clone())
                            .unwrap_or_else(|| "photo_lightcraft.jpg".to_string());
                        self.host.send(HostRequest::SaveToDevice { name, bytes: b });
                    }
                    None => {
                        self.set_status(
                            "That saved version is no longer stored.".to_string(),
                            true,
                        );
                    }
                }
                return;
            }
        }
        // A source blob that arrived for a thumbnail regeneration: render
        // the thumbnail from it and store it, instead of dropping it as a
        // superseded load. A pending open or export above owns the same
        // blob first, so a regeneration requested while one is in flight
        // simply waits for the next library load.
        if let Some((thumb_key_str, recipe)) = self.thumb_regen.remove(&key) {
            let Some(bytes) = bytes else { return };
            let still_there = if let Some(id_str) = thumb_key_str.strip_prefix("library/thumb/") {
                id_str.parse::<u64>().is_ok_and(|id| self.library.photo(id).is_some())
            } else if let Some(id_str) = thumb_key_str.strip_prefix("library/version-thumb/") {
                id_str.parse::<u64>().is_ok_and(|id| self.library.version(id).is_some())
            } else {
                false
            };
            if !still_there {
                return;
            }
            let Ok(photo) = Photo::open(&bytes) else { return };
            let analysis = crate::analysis::analyze(&photo.proxy);
            if let Some(thumb) = self.render_thumb_jpeg(&photo, &analysis, &recipe) {
                self.pending_thumbs.push((thumb_key_str.clone(), thumb.clone()));
                self.host.send(HostRequest::Put { key: thumb_key_str, bytes: thumb });
            }
            return;
        }
        // Anything else that arrived is a thumbnail blob. A load blob
        // whose key no longer matches a pending slot is a superseded
        // answer (a second tap overwrote the slot): it is ignored here,
        // never queued as a thumbnail.
        if key.starts_with("library/original/") || key.starts_with("library/version/") {
            return;
        }
        if let Some(b) = bytes {
            self.pending_thumbs.push((key, b));
        } else {
            // A stored thumbnail that is missing is regenerated from the
            // source blob instead of leaving "No preview" up forever.
            self.request_thumb_regen(&key);
        }
    }

    /// Adopt a successfully parsed index and fetch its thumbnails. This
    /// is the only place that clears the index write block and the
    /// load/save failure flags: a good parse is the proof the stored
    /// index (or its absence, on a first run) was read correctly.
    fn adopt_library(&mut self, lib: Library) {
        self.library = lib;
        self.library_loaded = true;
        self.good_index_loaded = true;
        self.index_write_blocked = false;
        self.index_load_failed = false;
        self.index_save_failed = false;
        for entry in self.library.recent() {
            let k = thumb_key(entry.id);
            self.host.send(HostRequest::Get { key: k });
        }
        for version in self.library.saved() {
            let k = version_thumb_key(version.id);
            self.host.send(HostRequest::Get { key: k });
        }
    }

    /// A stored index could not be read: keep the in-memory library untouched
    /// and try the backup copy. Index writes are blocked from here until
    /// the backup is adopted (only `adopt_library` clears the block), so
    /// nothing written while recovery is in flight can overwrite the
    /// stored index with an incomplete library.
    fn start_corrupt_index_recovery(&mut self, message: &str) {
        self.index_write_blocked = true;
        self.awaiting_index_backup = true;
        self.set_status(
            format!("{message} Trying the backup copy. Your stored photos are still there."),
            true,
        );
        self.host.send(HostRequest::Get { key: INDEX_BACKUP_KEY.to_string() });
    }

    /// Neither the index nor its backup could be read: keep the in-memory
    /// library, show the error, and block index writes so the unreadable
    /// stored index is never overwritten with an empty one. The
    /// persistent load error and its Retry button stay up until a
    /// readable index loads.
    fn finish_corrupt_index(&mut self) {
        self.awaiting_index_backup = false;
        self.library_loaded = true;
        self.index_write_blocked = true;
        self.index_load_failed = true;
        self.set_status(
            "The saved library index and its backup could not be read. Your stored photos are still there, but edits will not be saved to the library index until a readable index loads.".to_string(),
            true,
        );
    }

    fn open_loaded(&mut self, pending: PendingOpen, bytes: Option<Vec<u8>>) {
        let Some(bytes) = bytes else {
            self.set_status("That photo is no longer stored.".to_string(), true);
            return;
        };
        match pending {
            PendingOpen::Photo(id) => {
                let Some(entry) = self.library.photo(id).cloned() else {
                    self.set_status("That photo was removed.".to_string(), true);
                    return;
                };
                let recipe = entry.recipe.clone();
                let name = entry.name.clone();
                match Photo::open(&bytes) {
                    Ok(photo) => self.open_entry(Some(id), None, id, name, bytes, photo, recipe),
                    Err(e) => self.set_status(format!("{e}"), true),
                }
            }
            PendingOpen::Version(id) => {
                let Some(version) = self.library.version(id).cloned() else {
                    self.set_status("That saved version was removed.".to_string(), true);
                    return;
                };
                let recipe = version.recipe.clone();
                let source = version.photo;
                let name = version.name.clone();
                match Photo::open(&bytes) {
                    Ok(photo) => {
                        self.open_entry(None, Some(id), source, name, bytes, photo, recipe);
                    }
                    Err(e) => self.set_status(format!("{e}"), true),
                }
            }
        }
    }

    fn open_picked(&mut self, name: String, bytes: Vec<u8>) {
        let photo = match Photo::open(&bytes) {
            Ok(p) => p,
            Err(e) => {
                self.set_status(format!("{e}"), true);
                return;
            }
        };
        let analysis = crate::analysis::analyze(&photo.proxy);
        let now = self.host.now_ms();
        let id = self.library.add_photo(&name, photo.width, photo.height, bytes.len() as u64, now);
        self.host.send(HostRequest::Put { key: original_key(id), bytes: bytes.clone() });
        if let Some(thumb) = self.render_thumb_jpeg(&photo, &analysis, &Recipe::default()) {
            let key = thumb_key(id);
            self.pending_thumbs.push((key.clone(), thumb.clone()));
            self.host.send(HostRequest::Put { key, bytes: thumb });
        }
        let dropped = self.library.prune(Some(id));
        for key in dropped {
            self.host.send(HostRequest::Delete { key: key.clone() });
            self.thumbs.remove(&key);
        }
        if !self.save_library() {
            // The index is not loaded yet, recovery is in flight, or
            // writes are blocked: keep the pending write so the frame
            // flush retries it once a good index loads, instead of
            // dropping the new entry.
            self.dirty_library = true;
        }
        let display_name = self
            .library
            .photo(id)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| name.clone());
        self.open_entry(Some(id), None, id, display_name, bytes, photo, Recipe::default());
    }

    fn open_entry(
        &mut self,
        entry_id: Option<u64>,
        version_id: Option<u64>,
        source_photo_id: u64,
        name: String,
        bytes: Vec<u8>,
        photo: Photo,
        recipe: Recipe,
    ) {
        let analysis = crate::analysis::analyze(&photo.proxy);
        let advice = crate::advisor::advise(&analysis);
        if let Some(id) = entry_id {
            self.library.touch(id, self.host.now_ms());
            self.library.set_recipe(id, &recipe);
            self.save_library();
        }
        let status_name = name.clone();
        self.current = Some(OpenPhoto {
            entry_id,
            version_id,
            source_photo_id,
            name,
            bytes,
            photo,
            analysis,
            advice,
        });
        self.recipe = recipe;
        self.history.clear();
        self.gesture_key = None;
        self.tab = Tab::Auto;
        self.look_thumbs.clear();
        self.look_thumbs_for = String::new();
        self.screen = Screen::Editor;
        // Entering the editor disarms a two-step delete armed in the grid:
        // returning later must not delete on one tap.
        self.armed_remove = None;
        // A pending open that reached here succeeded; say so, instead of
        // leaving the "Opening photo." status up for the whole edit.
        self.set_status(format!("Opened {status_name}."), false);
    }

    /// Store the open entry's current recipe back into the library entry
    /// it came from, so edits are not lost when the editor is left for
    /// another photo or version. A saved version opened for further edits
    /// has no photo entry (`entry_id` is `None`); its recipe lives on the
    /// version entry in `library.versions` and is written there. No-op
    /// when nothing is open. The caller decides when the index is saved.
    fn store_current_recipe(&mut self) {
        let (entry_id, version_id) = match &self.current {
            Some(c) => (c.entry_id, c.version_id),
            None => return,
        };
        let recipe = self.recipe.clone();
        if let Some(id) = entry_id {
            self.library.set_recipe(id, &recipe);
        }
        if let Some(id) = version_id {
            if let Some(version) = self.library.versions.iter_mut().find(|v| v.id == id) {
                version.recipe = recipe;
            }
        }
    }

    /// Re-render the open entry's stored thumbnail with the recipe just
    /// stored in the library, and queue it for display and storage, so
    /// the grid shows the edited image instead of the pre-edit one.
    /// No-op when nothing is open or the entry has since been removed.
    /// Called after `store_current_recipe` in `leave_editor` and the
    /// `request_open_*` paths, which are the points where edits are
    /// stored; it is not called on every slider frame.
    fn refresh_stored_thumb(&mut self) {
        let (photo, analysis, key, recipe) = match &self.current {
            Some(c) => {
                let (key, recipe) = match (c.entry_id, c.version_id) {
                    (Some(id), _) => match self.library.photo(id) {
                        Some(e) => (thumb_key(id), e.recipe.clone()),
                        None => return,
                    },
                    (None, Some(id)) => match self.library.version(id) {
                        Some(v) => (version_thumb_key(id), v.recipe.clone()),
                        None => return,
                    },
                    (None, None) => return,
                };
                (c.photo.clone(), c.analysis.clone(), key, recipe)
            }
            None => return,
        };
        if let Some(thumb) = self.render_thumb_jpeg(&photo, &analysis, &recipe) {
            self.pending_thumbs.push((key.clone(), thumb.clone()));
            self.host.send(HostRequest::Put { key, bytes: thumb });
        }
    }

    /// Ask for the source blob a missing or corrupt stored thumbnail can
    /// be regenerated from. `thumb_key_str` is a `library/thumb/{id}` or
    /// `library/version-thumb/{id}` key. No-op for any other key, for an
    /// entry that no longer exists, or when a regeneration is already
    /// waiting on that source blob, so a bad thumbnail can never loop.
    fn request_thumb_regen(&mut self, thumb_key_str: &str) {
        let (source_key, recipe) = if let Some(id_str) = thumb_key_str.strip_prefix("library/thumb/") {
            let Ok(id) = id_str.parse::<u64>() else { return };
            let Some(entry) = self.library.photo(id) else { return };
            (original_key(id), entry.recipe.clone())
        } else if let Some(id_str) = thumb_key_str.strip_prefix("library/version-thumb/") {
            let Ok(id) = id_str.parse::<u64>() else { return };
            let Some(version) = self.library.version(id) else { return };
            (version_key(id), version.recipe.clone())
        } else {
            return;
        };
        if self.thumb_regen.contains_key(&source_key) {
            return;
        }
        self.thumb_regen.insert(source_key.clone(), (thumb_key_str.to_string(), recipe));
        self.host.send(HostRequest::Get { key: source_key });
    }

    fn leave_editor(&mut self) {
        self.store_current_recipe();
        self.refresh_stored_thumb();
        // Leaving the editor disarms a two-step delete armed in the grid:
        // returning later must not delete on one tap.
        self.armed_remove = None;
        self.dirty_library = true;
        self.screen = Screen::Library;
    }

    fn request_open_photo(&mut self, id: u64) {
        if self.current.as_ref().is_some_and(|c| c.entry_id == Some(id)) {
            self.screen = Screen::Editor;
            return;
        }
        // Leaving an open photo or version first stores its recipe in the
        // library, so edits to a saved version are not lost either.
        self.store_current_recipe();
        self.refresh_stored_thumb();
        self.save_library();
        self.pending_open = Some(PendingOpen::Photo(id));
        self.host.send(HostRequest::Get { key: original_key(id) });
        self.set_status("Opening photo.".to_string(), false);
    }

    fn request_open_version(&mut self, id: u64) {
        if self.current.as_ref().is_some_and(|c| c.version_id == Some(id)) {
            self.screen = Screen::Editor;
            return;
        }
        // Leaving an open photo or version first stores its recipe in the
        // library, as opening another photo does.
        self.store_current_recipe();
        self.refresh_stored_thumb();
        self.save_library();
        self.pending_open = Some(PendingOpen::Version(id));
        self.host.send(HostRequest::Get { key: version_key(id) });
        self.set_status("Opening saved version.".to_string(), false);
    }

    fn toggle_keep(&mut self, id: u64) {
        let kept = self.library.photo(id).map(|p| p.kept).unwrap_or(false);
        self.library.set_kept(id, !kept);
        self.save_library();
        let name = self.library.photo(id).map(|p| p.name.clone()).unwrap_or_default();
        if kept {
            self.set_status(format!("{name} will no longer be kept."), false);
        } else {
            self.set_status(format!("{name} is kept."), false);
        }
    }

    /// Two step removal: the first call arms it, the second deletes.
    fn remove_photo_action(&mut self, id: u64) {
        if self.armed_remove == Some(RemovalTarget::Photo(id)) {
            self.armed_remove = None;
            let name = self.library.photo(id).map(|p| p.name.clone()).unwrap_or_default();
            for key in self.library.remove_photo(id) {
                self.host.send(HostRequest::Delete { key: key.clone() });
                self.thumbs.remove(&key);
            }
            if self.current.as_ref().is_some_and(|c| c.entry_id == Some(id)) {
                self.current = None;
                self.screen = Screen::Library;
            }
            self.save_library();
            self.set_status(format!("Removed {name}."), false);
        } else {
            self.armed_remove = Some(RemovalTarget::Photo(id));
            self.set_status("Tap Remove again to delete this photo.".to_string(), false);
        }
    }

    fn remove_version_action(&mut self, id: u64) {
        if self.armed_remove == Some(RemovalTarget::Version(id)) {
            self.armed_remove = None;
            let name = self.library.version(id).map(|v| v.name.clone()).unwrap_or_default();
            for key in self.library.remove_version(id) {
                self.host.send(HostRequest::Delete { key: key.clone() });
                self.thumbs.remove(&key);
            }
            if self.current.as_ref().is_some_and(|c| c.version_id == Some(id)) {
                self.current = None;
                self.screen = Screen::Library;
            }
            self.save_library();
            self.set_status(format!("Deleted saved version {name}."), false);
        } else {
            self.armed_remove = Some(RemovalTarget::Version(id));
            self.set_status("Tap Delete again to delete this saved version.".to_string(), false);
        }
    }

    /// Apply one tip's one tap fix, with undo history. Advice only tips have
    /// no action and are never given an Apply button.
    fn apply_tip(&mut self, tip: &Tip) {
        let Some(action) = &tip.action else {
            self.set_status("That is guidance only, with nothing to apply.".to_string(), false);
            return;
        };
        let key = format!("tip:{}", tip.id);
        self.edit_recipe(&key, |r| {
            action.apply(r);
        });
        self.set_status(format!("Applied: {}", tip.title), false);
    }

    fn set_look(&mut self, look_id: Option<String>) {
        let name = match &look_id {
            Some(id) => look(id).map(|l| l.name.to_string()).unwrap_or_else(|| id.clone()),
            None => "Original".to_string(),
        };
        self.edit_recipe("look", |r| {
            r.look = look_id;
        });
        self.set_status(format!("Look: {name}."), false);
    }

    /// Record the recipe as it is now, apply a change, and seal the step.
    /// A change that leaves the recipe identical (re-picking the current
    /// aspect, resetting an already default group, re-applying an
    /// already applied tip) records nothing: `History::record` would
    /// push a no-op step and clear the redo stack for it.
    fn edit_recipe(&mut self, key: &str, apply: impl FnOnce(&mut Recipe)) {
        let before = self.recipe.clone();
        let mut next = before.clone();
        apply(&mut next);
        if next == before {
            return;
        }
        self.history.record(&before, key);
        self.recipe = next;
        self.history.seal();
        self.gesture_key = None;
        self.dirty_library = true;
    }

    fn undo(&mut self) {
        let current = self.recipe.clone();
        // A gesture left open by a drag that ended without a stop event must
        // not survive the undo: the next drag would reuse its key and skip
        // recording, losing the undo step for that drag (W11).
        self.gesture_key = None;
        if let Some(prev) = self.history.undo(&current) {
            self.recipe = prev;
            self.dirty_library = true;
            self.set_status("Undid the last change.".to_string(), false);
        } else {
            self.set_status("Nothing to undo.".to_string(), false);
        }
    }

    fn redo(&mut self) {
        let current = self.recipe.clone();
        self.gesture_key = None;
        if let Some(next) = self.history.redo(&current) {
            self.recipe = next;
            self.dirty_library = true;
            self.set_status("Redid the last change.".to_string(), false);
        } else {
            self.set_status("Nothing to redo.".to_string(), false);
        }
    }

    fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    fn look_strength_enabled(&self) -> bool {
        self.recipe.look.is_some()
    }

    fn focus_background_enabled(&self) -> bool {
        self.recipe.focus.amount > 0.0
    }

    fn current_settings(&self) -> DevelopSettings {
        let (recipe, analysis) = match &self.current {
            Some(c) => (self.recipe.clone(), c.analysis.clone()),
            None => (Recipe::default(), Analysis::default()),
        };
        compose(&recipe, &analysis)
    }

    /// Render a full resolution JPEG of the current edit, or an error message.
    fn render_full_jpeg(&self) -> Result<Vec<u8>, String> {
        let Some(current) = &self.current else {
            return Err("No photo is open.".to_string());
        };
        let settings = self.current_settings();
        export_jpeg(&current.bytes, &settings, JPEG_QUALITY).map_err(|e| format!("Export failed: {e}"))
    }

    /// Save the current edit as a full resolution version kept by the app.
    fn save_version(&mut self) {
        let Some(current) = &self.current else {
            self.set_status("No photo is open.".to_string(), true);
            return;
        };
        let source = current.source_photo_id;
        let recipe = self.recipe.clone();
        let name = current.name.clone();
        let jpeg = match self.render_full_jpeg() {
            Ok(j) => j,
            Err(e) => {
                self.set_status(e, true);
                return;
            }
        };
        let settings = self.current_settings();
        let (width, height) = match &self.current {
            Some(c) => export_size(c.photo.width, c.photo.height, &settings),
            None => (0, 0),
        };
        let now = self.host.now_ms();
        let id = self.library.add_version(source, width, height, jpeg.len() as u64, &recipe, now);
        self.host.send(HostRequest::Put { key: version_key(id), bytes: jpeg });
        if let Some(current) = &self.current {
            let analysis = current.analysis.clone();
            let recipe_now = self.recipe.clone();
            if let Some(thumb) = self.render_thumb_jpeg(&current.photo, &analysis, &recipe_now) {
                let key = version_thumb_key(id);
                self.pending_thumbs.push((key.clone(), thumb.clone()));
                self.host.send(HostRequest::Put { key, bytes: thumb });
            }
        }
        let version_name = self
            .library
            .version(id)
            .map(|v| v.name.clone())
            .unwrap_or_else(|| version_file_name(&name, 1));
        self.save_library();
        // The version bytes are still being written by the host: claim only
        // that saving started. A failed write surfaces through the Failed
        // event; there is no success event for a stored blob.
        self.set_status(
            format!("Saving version {version_name} at {width} by {height}. Saved versions are kept."),
            false,
        );
    }

    /// Save the current edit through the host (in the phone shell, the
    /// app cache via the Filesystem plugin; in a browser, a download).
    /// This is not a library version.
    fn export_to_device(&mut self) {
        let jpeg = match self.render_full_jpeg() {
            Ok(j) => j,
            Err(e) => {
                self.set_status(e, true);
                return;
            }
        };
        let (name, source_id, count, now) = match &self.current {
            Some(c) => (
                c.name.clone(),
                c.source_photo_id,
                self.library.versions.iter().filter(|v| v.photo == c.source_photo_id).count() + 1,
                self.host.now_ms(),
            ),
            None => (String::new(), 0, 1, self.host.now_ms()),
        };
        let file_name = export_file_name(&name, source_id, count, now);
        self.host.send(HostRequest::SaveToDevice { name: file_name.clone(), bytes: jpeg });
        self.set_status(format!("Exporting {file_name}."), false);
    }

    /// Open the system share sheet with a full resolution render.
    fn share(&mut self) {
        let jpeg = match self.render_full_jpeg() {
            Ok(j) => j,
            Err(e) => {
                self.set_status(e, true);
                return;
            }
        };
        let (name, source_id, count, now) = match &self.current {
            Some(c) => (
                c.name.clone(),
                c.source_photo_id,
                self.library.versions.iter().filter(|v| v.photo == c.source_photo_id).count() + 1,
                self.host.now_ms(),
            ),
            None => (String::new(), 0, 1, self.host.now_ms()),
        };
        let file_name = export_file_name(&name, source_id, count, now);
        self.host.send(HostRequest::Share { name: file_name, bytes: jpeg });
        self.set_status("Opening the share sheet.".to_string(), false);
    }

    /// A small JPEG of a photo with a recipe, for a library thumbnail.
    fn render_thumb_jpeg(
        &self,
        photo: &Photo,
        analysis: &Analysis,
        recipe: &Recipe,
    ) -> Option<Vec<u8>> {
        let settings = compose(recipe, analysis);
        let rendered = photo.render(&settings, THUMB_EDGE, THUMB_EDGE, Quality::Draft);
        encode_jpeg(&rendered, 80).ok()
    }

    /// Identity of the open photo, for cache keys. A new import gets a new
    /// library id, so a changed photo never reuses another photo's preview.
    fn photo_key(&self) -> String {
        match &self.current {
            Some(c) => format!("{:?}/{:?}/{}", c.entry_id, c.version_id, c.name),
            None => String::new(),
        }
    }

    /// Render (or reuse) the main preview for the area it will fill.
    /// Draft while dragging, full size on release, capped so a phone WebView
    /// is not asked for a 1600 pixel render on every frame.
    fn ensure_preview(
        &mut self,
        ctx: &egui::Context,
        area: egui::Vec2,
    ) -> Option<(egui::TextureId, [usize; 2])> {
        let (width, height) = match &self.current {
            Some(c) => (c.photo.width, c.photo.height),
            None => return None,
        };
        if area.x < 8.0 || area.y < 8.0 {
            return self.preview.as_ref().map(|p| (p.tex.id(), p.size));
        }
        let recipe = if self.holding_compare { Recipe::default() } else { self.recipe.clone() };
        let quality = if self.dragging { Quality::Draft } else { Quality::Full };
        let cap = if self.dragging { DRAFT_PREVIEW_EDGE } else { FULL_PREVIEW_EDGE };
        let ppp = ctx.pixels_per_point();
        let max_w = ((area.x * ppp).max(1.0).min(cap as f32)) as usize;
        let max_h = ((area.y * ppp).max(1.0).min(cap as f32)) as usize;
        let key = format!(
            "{}|{width}x{height}|{recipe:?}|{}|{max_w}x{max_h}|{quality:?}",
            self.photo_key(),
            self.holding_compare
        );
        if let Some(p) = &self.preview {
            if p.key == key {
                return Some((p.tex.id(), p.size));
            }
        }
        let (photo, analysis) = match &self.current {
            Some(c) => (c.photo.clone(), c.analysis.clone()),
            None => return None,
        };
        let settings = compose(&recipe, &analysis);
        let rendered = photo.render(&settings, max_w, max_h, quality);
        let image = &rendered.image;
        if image.width == 0 || image.height == 0 {
            return None;
        }
        let raw = image.as_bytes();
        let color = Arc::new(ColorImage::from_rgba_unmultiplied([image.width, image.height], &raw));
        let size = [image.width, image.height];
        match &mut self.preview {
            Some(p) => {
                p.tex.set(color, TextureOptions::LINEAR);
                p.key = key;
                p.size = size;
                Some((p.tex.id(), p.size))
            }
            None => {
                let tex =
                    ctx.load_texture("lightcraft-mobile-preview", color, TextureOptions::LINEAR);
                self.preview = Some(Preview { key, tex, size });
                self.preview.as_ref().map(|p| (p.tex.id(), p.size))
            }
        }
    }

    /// Render (or reuse) one look's thumbnail from the small proxy.
    fn ensure_look_thumb(
        &mut self,
        ctx: &egui::Context,
        look_id: &str,
    ) -> Option<(egui::TextureId, [usize; 2])> {
        let key = self.photo_key();
        if self.look_thumbs_for != key {
            self.look_thumbs.clear();
            self.look_thumbs_for = key;
        }
        if let Some((tex, size)) = self.look_thumbs.get(look_id) {
            return Some((tex.id(), *size));
        }
        let (photo, analysis) = match &self.current {
            Some(c) => (c.photo.clone(), c.analysis.clone()),
            None => return None,
        };
        let recipe = Recipe {
            look: Some(look_id.to_string()),
            look_strength: 100.0,
            ..Recipe::default()
        };
        let settings = compose(&recipe, &analysis);
        let rendered = photo.render_proxy(&settings, LOOK_THUMB_EDGE);
        let image = &rendered.image;
        if image.width == 0 || image.height == 0 {
            return None;
        }
        let raw = image.as_bytes();
        let color = Arc::new(ColorImage::from_rgba_unmultiplied([image.width, image.height], &raw));
        let size = [image.width, image.height];
        let name = format!("lightcraft-mobile-look-{look_id}");
        let tex = ctx.load_texture(name, color, TextureOptions::LINEAR);
        self.look_thumbs.insert(look_id.to_string(), (tex, size));
        self.look_thumbs.get(look_id).map(|(tex, size)| (tex.id(), *size))
    }

    /// A linear channel value a damaged decode can be trusted with:
    /// finite, non-negative, bounded.
    fn clean_channel(v: f32) -> f32 {
        if v.is_finite() {
            v.clamp(0.0, 64.0)
        } else {
            0.0
        }
    }

    fn upload_pending_thumbs(&mut self, ctx: &egui::Context) {
        let pending = std::mem::take(&mut self.pending_thumbs);
        for (key, bytes) in pending {
            // A stored thumbnail is already a finished render: its JPEG
            // is shown exactly as decoded, never run through compose()
            // again, or the edit would be applied twice. Only the
            // working-space (linear Rec.2020) to display sRGB conversion
            // runs, as analysis does for its display values. A photo
            // whose thumbnail is not yet stored simply has no texture
            // until one is rendered and stored; nothing is rendered
            // live from the original here.
            let photo = match Photo::open(&bytes) {
                Ok(p) => p,
                Err(_) => {
                    // A stored thumbnail that cannot be decoded is
                    // regenerated from the source blob instead of being
                    // silently dropped, which left "No preview" up.
                    self.request_thumb_regen(&key);
                    continue;
                }
            };
            let preview = &photo.preview;
            if preview.width == 0
                || preview.height == 0
                || preview.data.len() != preview.width * preview.height
            {
                self.request_thumb_regen(&key);
                continue;
            }
            let matrix = REC2020.to_space(&SRGB);
            let image = preview.map(|p| {
                let q = matrix.apply_f32([
                    Self::clean_channel(p[0]),
                    Self::clean_channel(p[1]),
                    Self::clean_channel(p[2]),
                ]);
                [encode_srgb8(q[0]), encode_srgb8(q[1]), encode_srgb8(q[2]), 255]
            });
            if image.width == 0 || image.height == 0 {
                continue;
            }
            let raw = image.as_bytes();
            let color =
                Arc::new(ColorImage::from_rgba_unmultiplied([image.width, image.height], &raw));
            let tex = ctx.load_texture(key.clone(), color, TextureOptions::LINEAR);
            self.thumbs.insert(key, (tex, [image.width, image.height]));
        }
    }
}

/// Fit an image inside an area, keeping its aspect ratio.
fn fit_size(area: egui::Vec2, image: [usize; 2]) -> egui::Vec2 {
    if area.x <= 0.0 || area.y <= 0.0 || image[0] == 0 || image[1] == 0 {
        return egui::Vec2::ZERO;
    }
    let scale = (area.x / image[0] as f32).min(area.y / image[1] as f32);
    egui::vec2(image[0] as f32 * scale, image[1] as f32 * scale)
}

fn format_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1_048_576.0)
    }
}

fn status_label(ui: &mut egui::Ui, app: &App) {
    if app.status.is_empty() {
        return;
    }
    let color = if app.status_error {
        egui::Color32::from_rgb(220, 80, 80)
    } else {
        egui::Color32::from_rgb(120, 120, 120)
    };
    ui.label(egui::RichText::new(app.status.as_str()).small().color(color));
}

// ---------------------------------------------------------------------------
// Screens and panels. Buttons here either call a real action on `App` or are
// disabled with a reason label next to them; there are no placeholder buttons.
// ---------------------------------------------------------------------------

impl App {
    fn library_ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("LightCraft");
        ui.label("A quiet editor for your photos.");
        // While the library is loading or backup recovery is in flight,
        // adding a photo could be saved over the stored index with an
        // incomplete library, so Add photo stays disabled until the
        // library loads.
        let library_ready = self.library_loaded && !self.awaiting_index_backup;
        ui.add_enabled_ui(library_ready, |ui| {
            if ui
                .add_sized(
                    egui::vec2(ui.available_width(), MIN_TOUCH + 4.0),
                    egui::Button::new("Add photo"),
                )
                .clicked()
            {
                // No picker-open guard: cancelling the browser picker fires no
                // event in most browsers, so a guard would deadlock Add photo
                // until restart. Opening a second picker input is harmless.
                self.host.send(HostRequest::PickPhoto);
                self.set_status("Choose a photo.".to_string(), false);
            }
        });
        ui.label(format!(
            "Keeping {} photos, {} stored. Saved versions are always kept. Unkept photos beyond the {} most recent are let go.",
            self.library.photos.len(),
            format_bytes(self.library.stored_bytes()),
            KEEP_RECENT
        ));
        if !self.library_loaded {
            ui.label("Loading your library. Adding and changing photos is paused until it loads.");
        }
        status_label(ui, self);
        if self.index_load_failed || self.index_save_failed {
            if self.index_load_failed {
                ui.label(
                    egui::RichText::new(
                        "The saved library index could not be loaded. Your stored photos are still there.",
                    )
                    .small()
                    .color(egui::Color32::from_rgb(220, 80, 80)),
                );
            } else {
                ui.label(
                    egui::RichText::new(
                        "The library index could not be saved. Your edits are kept in this session.",
                    )
                    .small()
                    .color(egui::Color32::from_rgb(220, 80, 80)),
                );
            }
            if ui
                .add_sized(
                    egui::vec2(ui.available_width(), MIN_TOUCH),
                    egui::Button::new("Retry"),
                )
                .clicked()
            {
                self.retry_storage();
            }
        }
        let filter_w = ui.available_width();
        ui.horizontal(|ui| {
            for (tab, label) in [
                (LibraryTab::Photos, "Photos"),
                (LibraryTab::Kept, "Kept"),
                (LibraryTab::Versions, "Versions"),
            ] {
                if ui
                    .add_sized(
                        egui::vec2((filter_w - 16.0) / 3.0, MIN_TOUCH),
                        egui::Button::new(label).selected(self.library_tab == tab),
                    )
                    .clicked()
                {
                    self.library_tab = tab;
                    self.armed_remove = None;
                }
            }
        });
        // Snapshot what the grid needs, then draw from the snapshot so button
        // actions can borrow `self` freely.
        let (tiles, version_tiles, armed) = self.library_snapshot();
        let mut actions: Vec<GridAction> = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            match self.library_tab {
                LibraryTab::Versions => {
                    if version_tiles.is_empty() {
                        ui.label("No saved versions yet.");
                        ui.label(
                            "Open a photo, edit it, and choose Save version. Saved versions are full resolution JPEGs and are kept until you delete them.",
                        );
                    }
                    draw_version_grid(ui, &version_tiles, armed, &mut actions);
                }
                _ => {
                    if tiles.is_empty() {
                        ui.label("No photos yet.");
                        ui.label(
                            "Add a photo to start editing. Photos you keep stay here. Others are let go once the recent list fills up.",
                        );
                    }
                    draw_photo_grid(ui, &tiles, armed, &mut actions);
                }
            }
        });
        // Grid actions are dropped while the library is loading or backup
        // recovery is in flight: a keep, remove, or open applied to the
        // in-memory library before recovery finishes could delete blobs
        // the recovered index still names, or be saved over it.
        if library_ready {
            for action in actions {
                match action {
                    GridAction::OpenPhoto(id) => self.request_open_photo(id),
                    GridAction::ToggleKeep(id) => self.toggle_keep(id),
                    GridAction::RemovePhoto(id) => self.remove_photo_action(id),
                    GridAction::OpenVersion(id) => self.request_open_version(id),
                    GridAction::ExportVersion(id) => {
                        self.pending_export_version = Some(id);
                        self.host.send(HostRequest::Get { key: version_key(id) });
                        self.set_status("Getting the saved version.".to_string(), false);
                    }
                    GridAction::DeleteVersion(id) => self.remove_version_action(id),
                }
            }
        }
    }

    /// Owned snapshots of the visible library rows, with thumbnail textures.
    fn library_snapshot(
        &self,
    ) -> (
        Vec<(EntryView, Option<(egui::TextureId, [usize; 2])>)>,
        Vec<(VersionView, Option<(egui::TextureId, [usize; 2])>)>,
        Option<RemovalTarget>,
    ) {
        let photos: Vec<EntryView> = match self.library_tab {
            LibraryTab::Kept => self
                .library
                .kept()
                .into_iter()
                .map(|e| EntryView {
                    id: e.id,
                    name: e.name.clone(),
                    width: e.width,
                    height: e.height,
                    kept: e.kept,
                })
                .collect(),
            _ => self
                .library
                .recent()
                .into_iter()
                .map(|e| EntryView {
                    id: e.id,
                    name: e.name.clone(),
                    width: e.width,
                    height: e.height,
                    kept: e.kept,
                })
                .collect(),
        };
        let tiles = photos
            .into_iter()
            .map(|v| {
                let tex = self
                    .thumbs
                    .get(&thumb_key(v.id))
                    .map(|(h, size)| (h.id(), *size));
                (v, tex)
            })
            .collect();
        let versions: Vec<(VersionView, Option<(egui::TextureId, [usize; 2])>)> = self
            .library
            .saved()
            .into_iter()
            .map(|v| {
                let view = VersionView { id: v.id, name: v.name.clone(), width: v.width, height: v.height };
                let tex = self
                    .thumbs
                    .get(&version_thumb_key(v.id))
                    .map(|(h, size)| (h.id(), *size));
                (view, tex)
            })
            .collect();
        (tiles, versions, self.armed_remove)
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui) -> (bool, bool) {
        let (name, dims) = match &self.current {
            Some(c) => (c.name.clone(), format!("{} by {}", c.photo.width, c.photo.height)),
            None => return (false, false),
        };
        let ctx = ui.ctx().clone();
        // Top bar: back, name, undo, redo.
        let top_w = ui.available_width();
        ui.horizontal(|ui| {
            if ui
                .add_sized(egui::vec2(MIN_TOUCH, MIN_TOUCH), egui::Button::new("Library"))
                .clicked()
            {
                self.leave_editor();
            }
            // Library 44 + gap 8 + Undo 44 + gap 8 + Redo 44 + gap 8:
            // the horizontal layout inserts 8pt between each widget, so
            // the name gets exactly what is left.
            let name_w = (top_w - MIN_TOUCH - 8.0 - MIN_TOUCH - 8.0 - MIN_TOUCH - 8.0).max(40.0);
            ui.add_sized(
                egui::vec2(name_w, MIN_TOUCH),
                egui::Label::new(egui::RichText::new(name.as_str()).size(16.0)).truncate(),
            );
            let can_undo = self.can_undo();
            let undo_resp = ui.add_enabled_ui(can_undo, |ui| {
                ui.add_sized(egui::vec2(MIN_TOUCH, MIN_TOUCH), egui::Button::new("Undo"))
            });
            if undo_resp.inner.clicked() {
                self.undo();
            }
            let can_redo = self.can_redo();
            let redo_resp = ui.add_enabled_ui(can_redo, |ui| {
                ui.add_sized(egui::vec2(MIN_TOUCH, MIN_TOUCH), egui::Button::new("Redo"))
            });
            if redo_resp.inner.clicked() {
                self.redo();
            }
        });
        if !self.can_undo() && !self.can_redo() {
            ui.label(egui::RichText::new("Nothing to undo or redo yet.").small());
        }
        ui.label(egui::RichText::new(dims.as_str()).small());
        // Action row: save a version, export, share, hold for original.
        let action_w = ui.available_width();
        let button_w = ((action_w - 24.0) / 4.0).max(60.0);
        let mut holding = false;
        ui.horizontal(|ui| {
            if ui
                .add_sized(egui::vec2(button_w, MIN_TOUCH), egui::Button::new("Save version"))
                .clicked()
            {
                self.save_version();
            }
            if ui
                .add_sized(egui::vec2(button_w, MIN_TOUCH), egui::Button::new("Export"))
                .clicked()
            {
                self.export_to_device();
            }
            if ui
                .add_sized(egui::vec2(button_w, MIN_TOUCH), egui::Button::new("Share"))
                .clicked()
            {
                self.share();
            }
            let resp = ui.add_sized(
                egui::vec2(button_w, MIN_TOUCH),
                egui::Button::new("Compare").selected(self.compare_latched),
            );
            let pressed = resp.is_pointer_button_down_on();
            if pressed && self.compare_press_ms.is_none() {
                self.compare_press_ms = Some(self.host.now_ms());
            }
            if resp.clicked() {
                // A quick tap latches compare on or off; a hold (300ms or
                // more) compares only while held and leaves the latch
                // alone, so releasing a hold never sticks compare on.
                let elapsed = self.compare_press_ms.map(|start| self.host.now_ms() - start);
                if elapsed.is_none_or(|ms| ms < 300.0) {
                    self.compare_latched = !self.compare_latched;
                }
            }
            if !pressed {
                self.compare_press_ms = None;
            }
            holding = pressed || self.compare_latched;
        });
        ui.label("Save version keeps a full resolution JPEG in the app. Export saves a copy in the app cache; use Share to move it out to Photos or Files.");
        status_label(ui, self);
        // Photo area, tab bar and panel share what is left. The panel is
        // 40% of the height (clamped 120..340pt), and it never takes so
        // much that less than 120pt is left for the photo after the top
        // bar, action row, tab bar and spacing (`reserved`): that guard
        // is what keeps the photo out of an 80pt sliver in landscape.
        // Portrait 390x844: panel 337.6, photo 346.4. Landscape 844x390:
        // panel 120, photo 110. (`reserved` still undercounts the label
        // rows above the photo; the panel is scrollable, so any spill
        // there clips inside the panel, not the photo.)
        let avail = ui.available_size();
        let reserved = MIN_TOUCH + MIN_TOUCH + TAB_BAR_HEIGHT + 24.0;
        let panel_h = (avail.y * 0.40).clamp(120.0, 340.0).min((avail.y - reserved - 120.0).max(120.0));
        let photo_h = (avail.y - reserved - panel_h).max(80.0);
        let preview = self.ensure_preview(&ctx, egui::vec2(avail.x, photo_h));
        ui.allocate_ui(egui::vec2(avail.x, photo_h), |ui| match preview {
            Some((id, size)) => {
                let fit = fit_size(ui.available_size(), size);
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new((id, fit)));
                });
            }
            None => {
                ui.centered_and_justified(|ui| {
                    ui.label("Preview is loading.");
                });
            }
        });
        // Bottom tab bar.
        let tab_w = ui.available_width();
        let mut any_dragged = false;
        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Auto, "Auto"),
                (Tab::Looks, "Looks"),
                (Tab::Tune, "Tune"),
                (Tab::Focus, "Focus"),
                (Tab::Crop, "Crop"),
            ] {
                if ui
                    .add_sized(
                        egui::vec2((tab_w - 32.0) / 5.0, TAB_BAR_HEIGHT),
                        egui::Button::new(label).selected(self.tab == tab),
                    )
                    .clicked()
                {
                    self.tab = tab;
                }
            }
        });
        // The active panel, scrollable inside its fixed height.
        let panel_h = panel_h.max(120.0);
        ui.allocate_ui(egui::vec2(avail.x, panel_h), |ui| {
            egui::ScrollArea::vertical()
                .max_height(panel_h)
                .auto_shrink([false, true])
                .show(ui, |ui| match self.tab {
                    Tab::Auto => self.auto_panel(ui, &mut any_dragged),
                    Tab::Looks => self.looks_panel(ui, &mut any_dragged),
                    Tab::Tune => self.tune_panel(ui, &mut any_dragged),
                    Tab::Focus => self.focus_panel(ui, &mut any_dragged),
                    Tab::Crop => self.crop_panel(ui, &mut any_dragged),
                });
        });
        (any_dragged, holding)
    }

    fn auto_panel(&mut self, ui: &mut egui::Ui, any_dragged: &mut bool) {
        let (advice, scene_label) = match &self.current {
            Some(c) => (c.advice.clone(), c.analysis.scene.label().to_string()),
            None => return,
        };
        ui.heading("Auto");
        ui.label(format!("Scene: {scene_label}."));
        ui.label(advice.summary.as_str());
        if advice.tags.is_empty() {
            ui.label("No tags for this photo.");
        } else {
            ui.label(advice.tags.join("  ·  "));
        }
        let auto = self.recipe.auto;
        self.float_slider(
            ui,
            SliderSpec { label: "Auto strength", min: 0.0, max: 100.0, step: 1.0, key: "auto", reset_to: Some(0.0) },
            auto,
            any_dragged,
            |r, v| r.auto = v,
        );
        ui.label("Auto is an enhancement based on this photo. At 0 it is off. Auto strength scales only the Auto enhancement; a look, if one is picked, is then blended over it at the Look strength set in the Looks panel.");
        if let Some(id) = advice.suggested_look {
            if let Some(found) = look(id) {
                let name = found.name.to_string();
                let id_string = id.to_string();
                if ui
                    .add_sized(
                        egui::vec2(ui.available_width(), MIN_TOUCH),
                        egui::Button::new(format!("Try look: {name}")),
                    )
                    .clicked()
                {
                    self.set_look(Some(id_string));
                }
            }
        }
        ui.heading("Guidance");
        if advice.tips.is_empty() {
            ui.label(
                "No one tap fixes for this photo yet. The guidance that suggests fixes is still being built, so this panel only shows the summary above. Auto, Looks and Tune still work.",
            );
            return;
        }
        for tip in advice.tips {
            ui.label(egui::RichText::new(tip.title.as_str()).strong());
            ui.label(tip.detail.as_str());
            if tip.action.is_some() {
                if ui
                    .add_sized(
                        egui::vec2(ui.available_width(), MIN_TOUCH),
                        egui::Button::new("Apply this fix"),
                    )
                    .clicked()
                {
                    self.apply_tip(&tip);
                }
            } else {
                ui.label("Guidance only, with nothing to apply.");
            }
            ui.separator();
        }
    }

    fn looks_panel(&mut self, ui: &mut egui::Ui, any_dragged: &mut bool) {
        ui.heading("Looks");
        ui.label("Looks adapt to each photo, so the same look lands differently on different photos.");
        if self.look_strength_enabled() {
            let strength = self.recipe.look_strength;
            self.float_slider(
                ui,
                SliderSpec {
                    label: "Look strength",
                    min: 0.0,
                    max: 100.0,
                    step: 1.0,
                    key: "look_strength",
                    reset_to: Some(100.0),
                },
                strength,
                any_dragged,
                |r, v| r.look_strength = v,
            );
        } else {
            // Same row geometry as an enabled `float_slider` row (104pt
            // label, full-width 44pt slider), so the panel does not jump
            // when a look is picked.
            let mut value = self.recipe.look_strength;
            ui.horizontal(|ui| {
                ui.add_sized(egui::vec2(104.0, MIN_TOUCH), egui::Label::new("Look strength"));
                let slider_w = ui.available_width().max(60.0);
                ui.add_enabled_ui(false, |ui| {
                    ui.add_sized(
                        egui::vec2(slider_w, MIN_TOUCH),
                        egui::Slider::new(&mut value, 0.0..=100.0),
                    )
                });
            });
            ui.label("Pick a look first, then set its strength.");
        }
        ui.label("Look strength blends the selected look over the Auto result. Thumbnails show each look at full strength; the preview uses the Look strength above.");
        let ctx = ui.ctx().clone();
        // The Original tile plus one tile per look.
        let current_look = self.recipe.look.clone();
        let mut chosen: Option<Option<String>> = None;
        if ui
            .add_sized(
                egui::vec2(ui.available_width(), MIN_TOUCH),
                egui::Button::new("Original: no look").selected(current_look.is_none()),
            )
            .clicked()
        {
            chosen = Some(None);
        }
        if LOOKS.is_empty() {
            ui.label("No looks yet.");
        }
        for found in LOOKS {
            let thumb = self.ensure_look_thumb(&ctx, found.id);
            let is_current = current_look.as_deref() == Some(found.id);
            // Tapping a look's thumbnail picks the look, like its Use button.
            let thumb_clicked = ui
                .allocate_ui(egui::vec2(ui.available_width(), 150.0), |ui| match thumb {
                    Some((id, size)) => {
                        let fit = fit_size(egui::vec2(ui.available_width(), 84.0), size);
                        ui.centered_and_justified(|ui| {
                            ui.add(egui::Image::new((id, fit)).sense(egui::Sense::click()))
                        })
                        .inner
                        .clicked()
                    }
                    None => {
                        ui.centered_and_justified(|ui| {
                            ui.label("No preview");
                        });
                        false
                    }
                })
                .inner;
            if thumb_clicked {
                chosen = Some(Some(found.id.to_string()));
            }
            ui.label(egui::RichText::new(found.name).strong());
            ui.label(found.blurb);
            if ui
                .add_sized(
                    egui::vec2(ui.available_width(), MIN_TOUCH),
                    egui::Button::new(format!("Use {}", found.name)).selected(is_current),
                )
                .clicked()
            {
                chosen = Some(Some(found.id.to_string()));
            }
            ui.separator();
        }
        if let Some(next) = chosen {
            self.set_look(next);
        }
    }

    fn tune_panel(&mut self, ui: &mut egui::Ui, any_dragged: &mut bool) {
        ui.heading("Tune");
        ui.label("Drag for a fast draft preview. Let go for the full preview.");
        let group_w = ui.available_width();
        ui.horizontal(|ui| {
            for group in TuneGroup::ALL {
                if ui
                    .add_sized(
                        egui::vec2((group_w - 24.0) / 4.0, MIN_TOUCH),
                        egui::Button::new(group.label()).selected(self.tune_group == group),
                    )
                    .clicked()
                {
                    self.tune_group = group;
                }
            }
        });
        let group = self.tune_group;
        for control in TUNE_CONTROLS {
            if control.group != group {
                continue;
            }
            let current = (control.get)(&self.recipe.tune);
            let step = if control.id == "exposure" { 0.1 } else { 1.0 };
            let key = format!("tune:{}", control.id);
            self.float_slider(
                ui,
                SliderSpec {
                    label: control.label,
                    min: control.min,
                    max: control.max,
                    step,
                    key: &key,
                    reset_to: Some(0.0),
                },
                current,
                any_dragged,
                |r, v| (control.set)(&mut r.tune, v),
            );
        }
        if ui
            .add_sized(
                egui::vec2(ui.available_width(), MIN_TOUCH),
                egui::Button::new(format!("Reset {} sliders", group.label())),
            )
            .clicked()
        {
            let key = format!("tune:{}:reset", group.label());
            self.edit_recipe(&key, |r| {
                for control in TUNE_CONTROLS {
                    if control.group == group {
                        (control.set)(&mut r.tune, 0.0);
                    }
                }
            });
            self.set_status(format!("Reset {} sliders.", group.label()), false);
        }
    }

    fn focus_panel(&mut self, ui: &mut egui::Ui, any_dragged: &mut bool) {
        ui.heading("Focus");
        let (cx, cy, confidence) = match &self.current {
            Some(c) => (c.analysis.subject.cx, c.analysis.subject.cy, c.analysis.subject.confidence),
            None => return,
        };
        if confidence > 0.0 {
            ui.label(format!(
                "Subject near ({:.0}%, {:.0}%), confidence {:.0}%. Focus brightens and sharpens the subject and quiets the background.",
                cx * 100.0,
                cy * 100.0,
                confidence * 100.0
            ));
        } else {
            ui.label(
                "No subject stood out, so Focus centres on the middle of the photo. Focus brightens and sharpens the subject and quiets the background.",
            );
        }
        let amount = self.recipe.focus.amount;
        self.float_slider(
            ui,
            SliderSpec {
                label: "Focus",
                min: 0.0,
                max: 100.0,
                step: 1.0,
                key: "focus:amount",
                reset_to: Some(0.0),
            },
            amount,
            any_dragged,
            |r, v| r.focus.amount = v,
        );
        if self.focus_background_enabled() {
            let background = self.recipe.focus.background;
            self.float_slider(
                ui,
                SliderSpec {
                    label: "Background",
                    min: 0.0,
                    max: 100.0,
                    step: 1.0,
                    key: "focus:background",
                    reset_to: Some(50.0),
                },
                background,
                any_dragged,
                |r, v| r.focus.background = v,
            );
        } else {
            // Same row geometry as an enabled `float_slider` row, so the
            // panel does not jump when Focus is turned on.
            let mut value = self.recipe.focus.background;
            ui.horizontal(|ui| {
                ui.add_sized(egui::vec2(104.0, MIN_TOUCH), egui::Label::new("Background"));
                let slider_w = ui.available_width().max(60.0);
                ui.add_enabled_ui(false, |ui| {
                    ui.add_sized(
                        egui::vec2(slider_w, MIN_TOUCH),
                        egui::Slider::new(&mut value, 0.0..=100.0),
                    )
                });
            });
            ui.label("Turn Focus on first, then set how much the background is quieted.");
        }
        ui.label("Focus uses the detected subject. Tap to place the subject is not in this build.");
        if ui
            .add_sized(
                egui::vec2(ui.available_width(), MIN_TOUCH),
                egui::Button::new("Reset focus"),
            )
            .clicked()
        {
            self.edit_recipe("focus:reset", |r| {
                r.focus = crate::recipe::Focus::default();
            });
            self.set_status("Focus reset.".to_string(), false);
        }
    }

    fn crop_panel(&mut self, ui: &mut egui::Ui, any_dragged: &mut bool) {
        ui.heading("Crop");
        ui.label("Pick an aspect. The crop is placed around the subject when Smart crop is on.");
        let aspect_w = ui.available_width();
        let current_aspect = self.recipe.frame.aspect;
        let mut chosen_aspect: Option<Aspect> = None;
        for row in Aspect::ALL.chunks(3) {
            ui.horizontal(|ui| {
                for aspect in row {
                    if ui
                        .add_sized(
                            egui::vec2((aspect_w - 16.0) / 3.0, MIN_TOUCH),
                            egui::Button::new(aspect.label()).selected(current_aspect == *aspect),
                        )
                        .clicked()
                    {
                        chosen_aspect = Some(*aspect);
                    }
                }
            });
        }
        if let Some(aspect) = chosen_aspect {
            self.edit_recipe("frame:aspect", |r| {
                r.frame.aspect = aspect;
            });
        }
        let row_w = ui.available_width();
        ui.horizontal(|ui| {
            if ui
                .add_sized(
                    egui::vec2((row_w - 16.0) / 3.0, MIN_TOUCH),
                    egui::Button::new("Rotate"),
                )
                .clicked()
            {
                self.edit_recipe("frame:rotate", |r| {
                    r.frame.turns = (r.frame.turns.wrapping_add(1)) % 4;
                });
            }
            let flip_h = self.recipe.frame.flip_h;
            if ui
                .add_sized(
                    egui::vec2((row_w - 16.0) / 3.0, MIN_TOUCH),
                    egui::Button::new("Flip").selected(flip_h),
                )
                .clicked()
            {
                self.edit_recipe("frame:flip", |r| {
                    r.frame.flip_h = !r.frame.flip_h;
                });
            }
            let mut smart = self.recipe.frame.smart;
            let resp = ui.add_sized(
                egui::vec2((row_w - 16.0) / 3.0, MIN_TOUCH),
                egui::Checkbox::new(&mut smart, "Smart"),
            );
            if resp.changed() {
                let value = smart;
                self.edit_recipe("frame:smart", |r| {
                    r.frame.smart = value;
                });
            }
        });
        let straighten = self.recipe.frame.straighten;
        self.float_slider(
            ui,
            SliderSpec {
                label: "Straighten",
                min: -45.0,
                max: 45.0,
                step: 1.0,
                key: "frame:straighten",
                reset_to: Some(0.0),
            },
            straighten,
            any_dragged,
            |r, v| r.frame.straighten = v,
        );
        ui.label("Pinch and double tap zoom are not in this build. Crop, rotate and straighten are how you frame here.");
        if ui
            .add_sized(
                egui::vec2(ui.available_width(), MIN_TOUCH),
                egui::Button::new("Reset framing"),
            )
            .clicked()
        {
            self.edit_recipe("frame:reset", |r| {
                r.frame = crate::recipe::Frame::default();
            });
            self.set_status("Framing reset.".to_string(), false);
        }
    }

    /// One labelled slider row, at least [`MIN_TOUCH`] tall, with undo history
    /// that coalesces a whole drag into one step and an optional reset button.
    /// `apply` writes the dragged value into the recipe; it is only called
    /// when the slider actually changed, so the recipe is never rewritten
    /// on frames where the user did nothing.
    fn float_slider(
        &mut self,
        ui: &mut egui::Ui,
        spec: SliderSpec<'_>,
        current: f64,
        any_dragged: &mut bool,
        apply: impl Fn(&mut Recipe, f64),
    ) {
        let SliderSpec { label, min, max, step, key, reset_to } = spec;
        let mut value = current;
        ui.horizontal(|ui| {
            ui.add_sized(
                egui::vec2(104.0, MIN_TOUCH),
                egui::Label::new(label),
            );
            let avail = ui.available_width();
            let slider_w = if reset_to.is_some() { (avail - 52.0).max(60.0) } else { avail };
            let resp = ui.add_sized(
                egui::vec2(slider_w, MIN_TOUCH),
                egui::Slider::new(&mut value, min..=max).step_by(step),
            );
            if resp.dragged() {
                *any_dragged = true;
            }
            if resp.changed() {
                if self.gesture_key.as_deref() != Some(key) {
                    let before = self.recipe.clone();
                    self.history.record(&before, key);
                    self.gesture_key = Some(key.to_string());
                }
                apply(&mut self.recipe, value);
                self.dirty_library = true;
                if !resp.dragged() || resp.drag_stopped() {
                    self.history.seal();
                    self.gesture_key = None;
                }
            } else if resp.drag_stopped() && self.gesture_key.as_deref() == Some(key) {
                // Only this slider's own open gesture ends here: a stop event
                // from another control must not seal a gesture in progress.
                self.history.seal();
                self.gesture_key = None;
            }
            if let Some(reset) = reset_to {
                if ui
                    .add_sized(egui::vec2(MIN_TOUCH, MIN_TOUCH), egui::Button::new("Reset"))
                    .clicked()
                {
                    let reset_key = format!("{key}:reset");
                    self.edit_recipe(&reset_key, |r| apply(r, reset));
                }
            }
        });
    }
}

/// Grid columns for a width, guaranteeing 44pt buttons. Three buttons
/// in one row need 3*44 + 2*8 (gaps) = 148pt of tile width; when a tile
/// is narrower the buttons fall back to two rows (second value true).
/// Narrow phones get 2 columns, wider ones 4: at 390pt the old
/// `/130.0` rule gave 3 columns and 38.9pt buttons.
fn grid_layout(avail_w: f32) -> (usize, bool) {
    let cols = if avail_w < 560.0 { 2 } else { 4 };
    let tile_w = (avail_w - (cols as f32 - 1.0) * 8.0) / cols as f32;
    let one_row = (tile_w - 16.0) / 3.0 >= MIN_TOUCH;
    (cols, !one_row)
}

fn draw_photo_grid(
    ui: &mut egui::Ui,
    tiles: &[(EntryView, Option<(egui::TextureId, [usize; 2])>)],
    armed: Option<RemovalTarget>,
    actions: &mut Vec<GridAction>,
) {
    let avail_w = ui.available_width();
    let (cols, two_rows) = grid_layout(avail_w);
    let tile_w = (avail_w - (cols as f32 - 1.0) * 8.0) / cols as f32;
    let tile_h = tile_w * 0.75 + if two_rows { 152.0 } else { 100.0 };
    for row in tiles.chunks(cols) {
        ui.horizontal(|ui| {
            for (view, tex) in row {
                ui.allocate_ui(egui::vec2(tile_w, tile_h), |ui| {
                    if draw_tile_image(ui, tile_w, *tex) {
                        actions.push(GridAction::OpenPhoto(view.id));
                    }
                    ui.add(egui::Label::new(view.name.as_str()).truncate());
                    ui.label(egui::RichText::new(format!("{} by {}", view.width, view.height)).small());
                    // Buttons are at least 44pt wide: one row of three
                    // when the tile fits them, else two rows.
                    let bw = ((tile_w - 16.0) / 3.0).max(MIN_TOUCH);
                    let bw2 = ((tile_w - 8.0) / 2.0).max(MIN_TOUCH);
                    let keep_label = if view.kept { "Kept" } else { "Keep" };
                    let armed_here = armed == Some(RemovalTarget::Photo(view.id));
                    let remove_label = if armed_here { "Sure?" } else { "Remove" };
                    if two_rows {
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(egui::vec2(bw2, MIN_TOUCH), egui::Button::new("Open"))
                                .clicked()
                            {
                                actions.push(GridAction::OpenPhoto(view.id));
                            }
                            if ui
                                .add_sized(
                                    egui::vec2(bw2, MIN_TOUCH),
                                    egui::Button::new(keep_label).selected(view.kept),
                                )
                                .clicked()
                            {
                                actions.push(GridAction::ToggleKeep(view.id));
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    egui::vec2(tile_w, MIN_TOUCH),
                                    egui::Button::new(remove_label).selected(armed_here),
                                )
                                .clicked()
                            {
                                actions.push(GridAction::RemovePhoto(view.id));
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(egui::vec2(bw, MIN_TOUCH), egui::Button::new("Open"))
                                .clicked()
                            {
                                actions.push(GridAction::OpenPhoto(view.id));
                            }
                            if ui
                                .add_sized(
                                    egui::vec2(bw, MIN_TOUCH),
                                    egui::Button::new(keep_label).selected(view.kept),
                                )
                                .clicked()
                            {
                                actions.push(GridAction::ToggleKeep(view.id));
                            }
                            if ui
                                .add_sized(
                                    egui::vec2(bw, MIN_TOUCH),
                                    egui::Button::new(remove_label).selected(armed_here),
                                )
                                .clicked()
                            {
                                actions.push(GridAction::RemovePhoto(view.id));
                            }
                        });
                    }
                });
            }
        });
    }
}

fn draw_version_grid(
    ui: &mut egui::Ui,
    tiles: &[(VersionView, Option<(egui::TextureId, [usize; 2])>)],
    armed: Option<RemovalTarget>,
    actions: &mut Vec<GridAction>,
) {
    let avail_w = ui.available_width();
    let (cols, two_rows) = grid_layout(avail_w);
    let tile_w = (avail_w - (cols as f32 - 1.0) * 8.0) / cols as f32;
    let tile_h = tile_w * 0.75 + if two_rows { 152.0 } else { 100.0 };
    for row in tiles.chunks(cols) {
        ui.horizontal(|ui| {
            for (view, tex) in row {
                ui.allocate_ui(egui::vec2(tile_w, tile_h), |ui| {
                    if draw_tile_image(ui, tile_w, *tex) {
                        actions.push(GridAction::OpenVersion(view.id));
                    }
                    ui.add(egui::Label::new(view.name.as_str()).truncate());
                    ui.label(egui::RichText::new(format!("{} by {}", view.width, view.height)).small());
                    let bw = ((tile_w - 16.0) / 3.0).max(MIN_TOUCH);
                    let bw2 = ((tile_w - 8.0) / 2.0).max(MIN_TOUCH);
                    let armed_here = armed == Some(RemovalTarget::Version(view.id));
                    let delete_label = if armed_here { "Sure?" } else { "Delete" };
                    if two_rows {
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(egui::vec2(bw2, MIN_TOUCH), egui::Button::new("Open"))
                                .clicked()
                            {
                                actions.push(GridAction::OpenVersion(view.id));
                            }
                            if ui
                                .add_sized(egui::vec2(bw2, MIN_TOUCH), egui::Button::new("Export"))
                                .clicked()
                            {
                                actions.push(GridAction::ExportVersion(view.id));
                            }
                        });
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    egui::vec2(tile_w, MIN_TOUCH),
                                    egui::Button::new(delete_label).selected(armed_here),
                                )
                                .clicked()
                            {
                                actions.push(GridAction::DeleteVersion(view.id));
                            }
                        });
                    } else {
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(egui::vec2(bw, MIN_TOUCH), egui::Button::new("Open"))
                                .clicked()
                            {
                                actions.push(GridAction::OpenVersion(view.id));
                            }
                            if ui
                                .add_sized(egui::vec2(bw, MIN_TOUCH), egui::Button::new("Export"))
                                .clicked()
                            {
                                actions.push(GridAction::ExportVersion(view.id));
                            }
                            if ui
                                .add_sized(
                                    egui::vec2(bw, MIN_TOUCH),
                                    egui::Button::new(delete_label).selected(armed_here),
                                )
                                .clicked()
                            {
                                actions.push(GridAction::DeleteVersion(view.id));
                            }
                        });
                    }
                });
            }
        });
    }
}

/// Draw one tile image, clickable: tapping it opens the photo or saved
/// version, like the Open button. Returns whether it was clicked.
fn draw_tile_image(ui: &mut egui::Ui, width: f32, tex: Option<(egui::TextureId, [usize; 2])>) -> bool {
    let height = width * 0.75;
    ui.allocate_ui(egui::vec2(width, height), |ui| match tex {
        Some((id, size)) => {
            let fit = fit_size(ui.available_size(), size);
            ui.centered_and_justified(|ui| ui.add(egui::Image::new((id, fit)).sense(egui::Sense::click())))
                .inner
                .clicked()
        }
        None => {
            ui.centered_and_justified(|ui| {
                ui.label("No preview");
            });
            false
        }
    })
    .inner
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;

    use super::host::{Host, MemoryHost};
    use super::*;
    use lightcraft_pipeline::Quality;
    use lightcraft_raster::Rgb32f;

    /// A small JPEG of a procedural scene, as bytes a photo picker would hand
    /// over. Built the same way `photo.rs` tests build one.
    fn scene_jpeg() -> Vec<u8> {
        let image = lightcraft_scenes::demo_library()
            .into_iter()
            .next()
            .map(|s| s.render(320, 240))
            .unwrap_or_else(|| Rgb32f::new(320, 240));
        let photo = Photo::from_image(image);
        let rendered = photo.render(&DevelopSettings::default(), 320, 240, Quality::Full);
        encode_jpeg(&rendered, 90).unwrap_or_default()
    }

    /// Run frames through a headless context. A bare `Context::run_ui` has no
    /// renderer, so clearing the texture delta is the explicit handling (the
    /// same pattern the desktop UI's headless tests use).
    fn run_frames(ctx: &egui::Context, app: &mut App, count: usize) {
        for _ in 0..count {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(390.0, 844.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
            out.textures_delta.clear();
        }
    }

    fn open_scene(app: &mut App) {
        app.handle_event(HostEvent::Picked { name: "IMG_1.jpg".to_string(), bytes: scene_jpeg() });
    }

    /// A host whose storage the test can inspect, mirroring `MemoryHost`.
    #[derive(Default)]
    struct TestState {
        store: BTreeMap<String, Vec<u8>>,
        picks: VecDeque<(String, Vec<u8>)>,
        saved: Vec<(String, Vec<u8>)>,
        shared: Vec<String>,
        requests: Vec<String>,
    }

    struct TestHost {
        now: f64,
        state: Rc<RefCell<TestState>>,
        events: VecDeque<HostEvent>,
    }

    impl TestHost {
        fn new(state: Rc<RefCell<TestState>>) -> TestHost {
            TestHost { now: 1.0, state, events: VecDeque::new() }
        }
    }

    impl Host for TestHost {
        fn now_ms(&self) -> f64 {
            self.now
        }

        fn send(&mut self, request: HostRequest) {
            self.now += 1.0;
            let mut state = self.state.borrow_mut();
            match request {
                HostRequest::PickPhoto => {
                    state.requests.push("pick".to_string());
                    match state.picks.pop_front() {
                        Some((name, bytes)) => {
                            self.events.push_back(HostEvent::Picked { name, bytes });
                        }
                        None => self.events.push_back(HostEvent::PickCancelled),
                    }
                }
                HostRequest::Put { key, bytes } => {
                    state.requests.push(format!("put {key}"));
                    state.store.insert(key, bytes);
                }
                HostRequest::Get { key } => {
                    state.requests.push(format!("get {key}"));
                    let bytes = state.store.get(&key).cloned();
                    self.events.push_back(HostEvent::Loaded { key, bytes });
                }
                HostRequest::Delete { key } => {
                    state.requests.push(format!("delete {key}"));
                    state.store.remove(&key);
                }
                HostRequest::SaveToDevice { name, bytes } => {
                    state.requests.push(format!("save {name}"));
                    self.events.push_back(HostEvent::Saved {
                        name: name.clone(),
                        location: format!("Pictures/LightCraft/{name}"),
                    });
                    state.saved.push((name, bytes));
                }
                HostRequest::Share { name, .. } => {
                    state.requests.push(format!("share {name}"));
                    state.shared.push(name);
                }
            }
        }

        fn poll(&mut self) -> Vec<HostEvent> {
            self.events.drain(..).collect()
        }
    }

    fn app_with_test_host() -> (App, Rc<RefCell<TestState>>) {
        let state = Rc::new(RefCell::new(TestState::default()));
        let app = App::new(Box::new(TestHost::new(Rc::clone(&state))));
        (app, state)
    }

    #[test]
    fn empty_library_frames_without_panic() {
        let ctx = egui::Context::default();
        let mut app = App::new(Box::new(MemoryHost::default()));
        run_frames(&ctx, &mut app, 3);
        assert_eq!(app.screen, Screen::Library);
        assert!(app.library.photos.is_empty());
        assert!(app.library_loaded);
    }

    #[test]
    fn pick_opens_photo_and_pick_cancel_reports_status() {
        let ctx = egui::Context::default();
        let mut app = App::new(Box::new(MemoryHost::default()));
        open_scene(&mut app);
        assert_eq!(app.screen, Screen::Editor);
        assert!(app.current.is_some());
        assert_eq!(app.library.photos.len(), 1);
        assert!(app.status.contains("IMG_1.jpg"));
        run_frames(&ctx, &mut app, 2);

        let mut app = App::new(Box::new(MemoryHost::default()));
        app.handle_event(HostEvent::PickCancelled);
        assert_eq!(app.screen, Screen::Library);
        assert!(!app.status.is_empty());
        assert!(!app.status_error);
    }

    #[test]
    fn garbage_pick_is_an_error_not_a_crash() {
        let mut app = App::new(Box::new(MemoryHost::default()));
        app.handle_event(HostEvent::Picked { name: "bad.jpg".to_string(), bytes: b"not a photo".to_vec() });
        assert_eq!(app.screen, Screen::Library);
        assert!(app.library.photos.is_empty());
        assert!(app.status_error);
        assert!(!app.status.is_empty());
        app.handle_event(HostEvent::Picked { name: "empty.jpg".to_string(), bytes: Vec::new() });
        assert!(app.library.photos.is_empty());
    }

    #[test]
    fn every_tab_and_tune_group_renders() {
        let ctx = egui::Context::default();
        let mut app = App::new(Box::new(MemoryHost::default()));
        open_scene(&mut app);
        for tab in [Tab::Auto, Tab::Looks, Tab::Tune, Tab::Focus, Tab::Crop] {
            app.tab = tab;
            run_frames(&ctx, &mut app, 2);
        }
        app.tab = Tab::Tune;
        for group in TuneGroup::ALL {
            app.tune_group = group;
            run_frames(&ctx, &mut app, 1);
        }
        // Hold for original renders the unedited recipe; releasing renders
        // the edit again. Both frames must run without panic.
        app.holding_compare = true;
        run_frames(&ctx, &mut app, 1);
        app.holding_compare = false;
        run_frames(&ctx, &mut app, 1);
        assert_eq!(app.screen, Screen::Editor);
    }

    #[test]
    fn tip_apply_and_undo() {
        let mut app = App::new(Box::new(MemoryHost::default()));
        open_scene(&mut app);
        assert!(!app.can_undo());
        let tip = Tip {
            id: "exposure".to_string(),
            title: "Brighten".to_string(),
            detail: "Raise exposure a little.".to_string(),
            action: Some(crate::advisor::TipAction::Tune("exposure", 0.5)),
        };
        app.apply_tip(&tip);
        assert_eq!(app.recipe.tune.exposure, 0.5);
        assert!(app.can_undo());
        app.undo();
        assert_eq!(app.recipe.tune.exposure, 0.0);
        assert!(app.can_redo());
        app.redo();
        assert_eq!(app.recipe.tune.exposure, 0.5);
        // An advice only tip changes nothing and is not an error.
        let advice_only = Tip {
            id: "advice".to_string(),
            title: "Wait for the light".to_string(),
            detail: "Come back at golden hour.".to_string(),
            action: None,
        };
        app.apply_tip(&advice_only);
        assert_eq!(app.recipe.tune.exposure, 0.5);
    }

    #[test]
    fn save_version_stores_a_full_resolution_jpeg() {
        let (mut app, state) = app_with_test_host();
        open_scene(&mut app);
        app.recipe.tune.exposure = 0.5;
        app.save_version();
        assert_eq!(app.library.versions.len(), 1);
        let version = app.library.versions[0].clone();
        assert_eq!((version.width, version.height), (320, 240));
        let stored = state.borrow();
        let bytes = stored.store.get(&version_key(version.id));
        assert!(bytes.is_some());
        let bytes = bytes.map(|b| b.clone()).unwrap_or_default();
        drop(stored);
        let back = Photo::open(&bytes).unwrap_or_else(|_| Photo::from_image(Rgb32f::new(1, 1)));
        assert_eq!((back.width, back.height), (320, 240));
        // The version thumb was stored too, under its own key.
        assert!(state.borrow().store.contains_key(&version_thumb_key(version.id)));
    }

    #[test]
    fn export_sends_a_device_save_and_share_sends_a_share() {
        let (mut app, state) = app_with_test_host();
        open_scene(&mut app);
        app.export_to_device();
        {
            let stored = state.borrow();
            assert_eq!(stored.saved.len(), 1);
            assert!(stored.saved[0].0.ends_with(".jpg"));
            assert!(Photo::open(&stored.saved[0].1).is_ok());
        }
        app.share();
        assert_eq!(state.borrow().shared.len(), 1);
    }

    #[test]
    fn retention_keeps_kept_recent_and_protected() {
        let mut app = App::new(Box::new(MemoryHost::default()));
        let bytes = scene_jpeg();
        app.handle_event(HostEvent::Picked { name: "first.jpg".to_string(), bytes: bytes.clone() });
        let first = app.library.photos[0].id;
        app.toggle_keep(first);
        assert!(app.library.photo(first).map(|p| p.kept).unwrap_or(false));
        for i in 1..15 {
            app.handle_event(HostEvent::Picked {
                name: format!("photo-{i}.jpg"),
                bytes: bytes.clone(),
            });
        }
        // Kept plus protected plus the 11 other most recent unkept photos.
        assert_eq!(app.library.photos.len(), 2 + (KEEP_RECENT - 1));
        assert!(app.library.photo(first).is_some());
        let last = app.library.photos.iter().map(|p| p.id).max().unwrap_or(0);
        assert!(app.library.photo(last).is_some());
    }

    #[test]
    fn remove_is_two_step_and_deletes_storage_keys() {
        let (mut app, state) = app_with_test_host();
        open_scene(&mut app);
        let id = app.library.photos[0].id;
        app.remove_photo_action(id);
        assert!(app.library.photo(id).is_some());
        assert!(app.status.contains("again"));
        app.remove_photo_action(id);
        assert!(app.library.photo(id).is_none());
        assert_eq!(app.screen, Screen::Library);
        let stored = state.borrow();
        assert!(stored.requests.iter().any(|r| r == &format!("delete {}", original_key(id))));
    }

    #[test]
    fn disabled_helpers_follow_state() {
        let mut app = App::new(Box::new(MemoryHost::default()));
        open_scene(&mut app);
        assert!(!app.look_strength_enabled());
        assert!(!app.focus_background_enabled());
        app.set_look(Some("clear".to_string()));
        assert!(app.look_strength_enabled());
        app.edit_recipe("focus", |r| r.focus.amount = 50.0);
        assert!(app.focus_background_enabled());
        app.set_look(None);
        assert!(!app.look_strength_enabled());
    }

    #[test]
    fn fit_size_and_format_bytes() {
        assert_eq!(fit_size(egui::vec2(100.0, 100.0), [200, 100]), egui::vec2(100.0, 50.0));
        assert_eq!(fit_size(egui::vec2(100.0, 100.0), [100, 200]), egui::vec2(50.0, 100.0));
        assert_eq!(fit_size(egui::vec2(0.0, 100.0), [100, 100]), egui::Vec2::ZERO);
        assert_eq!(fit_size(egui::vec2(100.0, 100.0), [0, 100]), egui::Vec2::ZERO);
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(3_145_728), "3.0 MB");
    }

    #[test]
    fn stored_index_round_trips_through_loaded_events() {
        let (mut app, state) = app_with_test_host();
        // Let the startup index load answer arrive (no stored index yet)
        // before opening a photo: index writes are held back until the
        // library has loaded, so the stored index would otherwise be empty.
        let ctx = egui::Context::default();
        run_frames(&ctx, &mut app, 1);
        open_scene(&mut app);
        app.toggle_keep(app.library.photos[0].id);
        // A fresh app reading the stored index sees the same library.
        let index = state.borrow().store.get(INDEX_KEY).cloned().unwrap_or_default();
        let (mut fresh, _) = app_with_test_host();
        // The fresh app's own Get already queued a Loaded with no bytes;
        // feed it the stored index instead, as the host would.
        fresh.handle_event(HostEvent::Loaded { key: INDEX_KEY.to_string(), bytes: Some(index) });
        assert_eq!(fresh.library.photos.len(), 1);
        assert!(fresh.library.photos[0].kept);
        // Garbage in the index keeps the library and tries the backup copy,
        // instead of wiping the library and saving an empty index over it.
        fresh.handle_event(HostEvent::Loaded { key: INDEX_KEY.to_string(), bytes: Some(b"{{{".to_vec()) });
        assert_eq!(fresh.library.photos.len(), 1);
        assert!(fresh.status_error);
        assert!(fresh.awaiting_index_backup);
        // The backup is missing too: the library is kept and index writes
        // are blocked, so the corrupt stored index is never overwritten.
        fresh.handle_event(HostEvent::Loaded { key: INDEX_BACKUP_KEY.to_string(), bytes: None });
        assert_eq!(fresh.library.photos.len(), 1);
        assert!(fresh.index_write_blocked);
        let stored_before = state.borrow().store.get(INDEX_KEY).cloned();
        fresh.save_library();
        assert_eq!(state.borrow().store.get(INDEX_KEY).cloned(), stored_before);
        // A readable backup recovers the library instead.
        let mut fresh2 = App::new(Box::new(TestHost::new(Rc::clone(&state))));
        fresh2.handle_event(HostEvent::Loaded { key: INDEX_KEY.to_string(), bytes: Some(b"{{{".to_vec()) });
        let backup = state.borrow().store.get(INDEX_BACKUP_KEY).cloned();
        fresh2.handle_event(HostEvent::Loaded { key: INDEX_BACKUP_KEY.to_string(), bytes: backup });
        assert_eq!(fresh2.library.photos.len(), 1);
        assert!(fresh2.library.photos[0].kept);
        assert!(!fresh2.index_write_blocked);
    }

    #[test]
    fn failed_index_load_tries_backup_blocks_writes_and_retries() {
        let (mut app, state) = app_with_test_host();
        // The main index load fails: recovery tries the backup copy and
        // index writes are blocked, instead of showing an empty library
        // that a later save could persist over the stored index.
        app.handle_event(HostEvent::Failed {
            what: "storage".to_string(),
            message: format!("loading {INDEX_KEY}: boom"),
        });
        assert!(app.awaiting_index_backup);
        assert!(app.index_write_blocked);
        assert!(app.index_load_failed);
        // The backup load fails too: still blocked, with Retry available.
        app.handle_event(HostEvent::Failed {
            what: "storage".to_string(),
            message: format!("loading {INDEX_BACKUP_KEY}: boom"),
        });
        assert!(!app.awaiting_index_backup);
        assert!(app.index_write_blocked);
        assert!(app.index_load_failed);
        assert!(app.library_loaded);
        // Writes stay blocked: save_library sends nothing.
        let before = state.borrow().requests.len();
        app.save_library();
        assert_eq!(state.borrow().requests.len(), before);
        // Retry re-sends the Get for the main index.
        app.retry_storage();
        assert!(state
            .borrow()
            .requests
            .iter()
            .any(|r| r == &format!("get {INDEX_KEY}")));
        assert!(!app.library_loaded);
        // The retry load finds no stored index: adopting the empty
        // library clears the block.
        app.handle_event(HostEvent::Loaded { key: INDEX_KEY.to_string(), bytes: None });
        assert!(app.good_index_loaded);
        assert!(!app.index_write_blocked);
        assert!(!app.index_load_failed);
    }

    #[test]
    fn failed_index_save_keeps_dirty_and_retries() {
        let (mut app, state) = app_with_test_host();
        let index = app.library.to_json().into_bytes();
        app.handle_event(HostEvent::Loaded { key: INDEX_KEY.to_string(), bytes: Some(index) });
        assert!(app.good_index_loaded);
        assert!(!app.index_write_blocked);
        // A failed Put of the index keeps the pending write and raises
        // the persistent save error, instead of dropping the edit.
        app.dirty_library = true;
        app.handle_event(HostEvent::Failed {
            what: "storage".to_string(),
            message: format!("storing {INDEX_KEY}: disk full"),
        });
        assert!(app.dirty_library);
        assert!(app.index_save_failed);
        // Retry re-attempts the pending index write.
        app.retry_storage();
        assert!(state
            .borrow()
            .requests
            .iter()
            .any(|r| r == &format!("put {INDEX_KEY}")));
        assert!(!app.dirty_library);
        assert!(!app.index_save_failed);
    }
}
