//! What the app keeps: an index of opened photos (originals, recipes, thumbnails) and saved
//! full-resolution versions, with a retention policy so storage doesn't grow without bound.
//!
//! Pure data: the host stores each blob under the key this module names ([`original_key`],
//! [`thumb_key`], [`version_key`], [`version_thumb_key`]) and the index itself as JSON under
//! [`INDEX_KEY`]. The policy:
//! - a photo the user **keeps** (the bookmark) stays until they remove it;
//! - otherwise the [`KEEP_RECENT`] most recently opened photos stay, older ones are let go;
//! - every **saved version** (a full-resolution JPEG) stays until the user deletes it, even when
//!   its source photo was let go;
//! - past [`STORAGE_BUDGET`] bytes, the oldest un-kept photos go first.

use serde::{Deserialize, Serialize};

use crate::recipe::Recipe;

/// Storage key of the library index (JSON).
pub const INDEX_KEY: &str = "library/index.json";
/// Un-kept photos kept around (most recently opened first).
pub const KEEP_RECENT: usize = 12;
/// Soft cap on stored originals plus versions (bytes).
pub const STORAGE_BUDGET: u64 = 1_500_000_000;
/// Index schema version.
pub const VERSION: u32 = 1;
/// Longest display name kept.
const MAX_NAME: usize = 120;

pub fn original_key(id: u64) -> String {
    format!("library/original/{id}")
}
pub fn thumb_key(id: u64) -> String {
    format!("library/thumb/{id}")
}
pub fn version_key(id: u64) -> String {
    format!("library/version/{id}")
}
pub fn version_thumb_key(id: u64) -> String {
    format!("library/version-thumb/{id}")
}

/// One opened photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: u64,
    pub name: String,
    /// Milliseconds since the Unix epoch.
    pub added_ms: f64,
    pub opened_ms: f64,
    pub kept: bool,
    pub width: u32,
    pub height: u32,
    /// Size of the stored original.
    pub bytes: u64,
    /// The current edit.
    pub recipe: Recipe,
}

impl Default for Entry {
    fn default() -> Self {
        Entry { id: 0, name: String::new(), added_ms: 0.0, opened_ms: 0.0, kept: false, width: 0, height: 0, bytes: 0, recipe: Recipe::default() }
    }
}

/// One saved full-resolution version.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Version {
    pub id: u64,
    /// The photo it came from (may since have been let go).
    pub photo: u64,
    pub name: String,
    pub created_ms: f64,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    /// The recipe it was rendered with (so it can be reopened for further edits while the
    /// original is still kept).
    pub recipe: Recipe,
}

/// The library index.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub version: u32,
    pub next_id: u64,
    pub photos: Vec<Entry>,
    pub versions: Vec<Version>,
}

/// A display name from a file name: no directories, no control characters, bounded length.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() { "Photo".to_string() } else { trimmed.to_string() }
}

/// File name of a saved version: the photo's stem plus a suffix, `.jpg`.
pub fn version_file_name(photo_name: &str, n: usize) -> String {
    let name = clean_name(photo_name);
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => name.get(..i).unwrap_or(&name),
        _ => name.as_str(),
    };
    let safe: String = stem.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if n <= 1 { format!("{safe}_lightcraft.jpg") } else { format!("{safe}_lightcraft_{n}.jpg") }
}

impl Library {
    /// Parse a stored index; anything unreadable yields an empty library (never a panic).
    pub fn from_json(text: &str) -> Library {
        let mut lib: Library = serde_json::from_str(text).unwrap_or_default();
        lib.repair();
        lib
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// Make a hostile or stale index consistent: unique ids, `next_id` past every id.
    fn repair(&mut self) {
        self.version = VERSION;
        let mut seen = std::collections::HashSet::new();
        self.photos.retain(|p| seen.insert(p.id));
        let mut seen = std::collections::HashSet::new();
        self.versions.retain(|v| seen.insert(v.id));
        let max = self.photos.iter().map(|p| p.id).chain(self.versions.iter().map(|v| v.id)).max().unwrap_or(0);
        self.next_id = self.next_id.max(max.saturating_add(1)).max(1);
        for p in &mut self.photos {
            p.name = clean_name(&p.name);
        }
    }

    fn fresh_id(&mut self) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        id
    }

    pub fn photo(&self, id: u64) -> Option<&Entry> {
        self.photos.iter().find(|p| p.id == id)
    }

    pub fn photo_mut(&mut self, id: u64) -> Option<&mut Entry> {
        self.photos.iter_mut().find(|p| p.id == id)
    }

    pub fn version(&self, id: u64) -> Option<&Version> {
        self.versions.iter().find(|v| v.id == id)
    }

    /// Add an opened photo; returns its id.
    pub fn add_photo(&mut self, name: &str, width: u32, height: u32, bytes: u64, now_ms: f64) -> u64 {
        let id = self.fresh_id();
        self.photos.push(Entry { id, name: clean_name(name), added_ms: now_ms, opened_ms: now_ms, width, height, bytes, ..Entry::default() });
        id
    }

    /// Mark a photo as opened now (it moves to the front of Recent).
    pub fn touch(&mut self, id: u64, now_ms: f64) {
        if let Some(p) = self.photo_mut(id) {
            p.opened_ms = now_ms;
        }
    }

    pub fn set_kept(&mut self, id: u64, kept: bool) {
        if let Some(p) = self.photo_mut(id) {
            p.kept = kept;
        }
    }

    pub fn set_recipe(&mut self, id: u64, recipe: &Recipe) {
        if let Some(p) = self.photo_mut(id) {
            p.recipe = recipe.clone();
        }
    }

    /// Remove a photo; returns the storage keys to delete.
    pub fn remove_photo(&mut self, id: u64) -> Vec<String> {
        let before = self.photos.len();
        self.photos.retain(|p| p.id != id);
        if self.photos.len() == before { Vec::new() } else { vec![original_key(id), thumb_key(id)] }
    }

    /// Record a saved version; returns its id.
    pub fn add_version(&mut self, photo: u64, width: u32, height: u32, bytes: u64, recipe: &Recipe, now_ms: f64) -> u64 {
        let id = self.fresh_id();
        let base = self.photo(photo).map(|p| p.name.clone()).unwrap_or_else(|| "Photo".into());
        let n = self.versions.iter().filter(|v| v.photo == photo).count() + 1;
        let name = version_file_name(&base, n);
        self.versions.push(Version { id, photo, name, created_ms: now_ms, width, height, bytes, recipe: recipe.clone() });
        id
    }

    /// Remove a saved version; returns the storage keys to delete.
    pub fn remove_version(&mut self, id: u64) -> Vec<String> {
        let before = self.versions.len();
        self.versions.retain(|v| v.id != id);
        if self.versions.len() == before { Vec::new() } else { vec![version_key(id), version_thumb_key(id)] }
    }

    /// Photos, most recently opened first.
    pub fn recent(&self) -> Vec<&Entry> {
        let mut v: Vec<&Entry> = self.photos.iter().collect();
        v.sort_by(|a, b| b.opened_ms.total_cmp(&a.opened_ms).then(b.id.cmp(&a.id)));
        v
    }

    /// Kept photos, most recently opened first.
    pub fn kept(&self) -> Vec<&Entry> {
        self.recent().into_iter().filter(|p| p.kept).collect()
    }

    /// Saved versions, newest first.
    pub fn saved(&self) -> Vec<&Version> {
        let mut v: Vec<&Version> = self.versions.iter().collect();
        v.sort_by(|a, b| b.created_ms.total_cmp(&a.created_ms).then(b.id.cmp(&a.id)));
        v
    }

    /// Bytes of stored originals and versions.
    pub fn stored_bytes(&self) -> u64 {
        self.photos.iter().map(|p| p.bytes).chain(self.versions.iter().map(|v| v.bytes)).fold(0u64, u64::saturating_add)
    }

    /// Apply the retention policy, never letting go of `protect` (the photo open in the editor).
    /// Returns the storage keys to delete.
    pub fn prune(&mut self, protect: Option<u64>) -> Vec<String> {
        let mut drop_ids = Vec::new();
        let unkept: Vec<u64> = self.recent().into_iter().filter(|p| !p.kept && Some(p.id) != protect).map(|p| p.id).collect();
        let room = KEEP_RECENT.saturating_sub(usize::from(protect.is_some_and(|id| self.photo(id).is_some_and(|p| !p.kept))));
        drop_ids.extend(unkept.iter().skip(room).copied());
        // over budget: let go of the oldest remaining un-kept photos
        let mut total = self.stored_bytes();
        for p in &self.photos {
            if drop_ids.contains(&p.id) {
                total = total.saturating_sub(p.bytes);
            }
        }
        for id in unkept.iter().rev() {
            if total <= STORAGE_BUDGET {
                break;
            }
            if !drop_ids.contains(id) {
                drop_ids.push(*id);
                total = total.saturating_sub(self.photo(*id).map_or(0, |p| p.bytes));
            }
        }
        drop_ids.into_iter().flat_map(|id| self.remove_photo(id)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("/storage/DCIM/IMG_1.jpg"), "IMG_1.jpg");
        assert_eq!(clean_name("C:\\x\\a\u{0}b.png"), "ab.png");
        assert_eq!(clean_name("   "), "Photo");
        assert_eq!(clean_name(&"é".repeat(500)).chars().count(), MAX_NAME);
        assert_eq!(version_file_name("IMG 1.jpeg", 1), "IMG_1_lightcraft.jpg");
        assert_eq!(version_file_name(".hidden", 2), "_hidden_lightcraft_2.jpg");
    }

    #[test]
    fn garbage_index_is_empty_and_repaired() {
        assert_eq!(Library::from_json("{{{").photos.len(), 0);
        let lib = Library::from_json(r#"{"next_id":0,"photos":[{"id":5},{"id":5},{"id":2}],"versions":[{"id":9}]}"#);
        assert_eq!(lib.photos.len(), 2);
        assert_eq!(lib.next_id, 10);
    }

    #[test]
    fn round_trip() {
        let mut lib = Library::default();
        let id = lib.add_photo("a.jpg", 10, 20, 100, 1.0);
        let mut r = Recipe::default();
        r.tune.exposure = 0.5;
        lib.set_recipe(id, &r);
        lib.add_version(id, 10, 20, 50, &r, 2.0);
        let back = Library::from_json(&lib.to_json());
        assert_eq!((back.photos, back.versions, back.next_id), (lib.photos, lib.versions, lib.next_id));
    }

    #[test]
    fn retention_keeps_recent_kept_and_protected() {
        let mut lib = Library::default();
        let mut ids = Vec::new();
        for i in 0..20 {
            ids.push(lib.add_photo(&format!("{i}.jpg"), 1, 1, 10, f64::from(i)));
        }
        lib.set_kept(ids[0], true);
        let v = lib.add_version(ids[1], 1, 1, 10, &Recipe::default(), 50.0);
        let deleted = lib.prune(Some(ids[2]));
        // kept + protected + 11 more recent
        assert!(lib.photo(ids[0]).is_some());
        assert!(lib.photo(ids[2]).is_some());
        assert_eq!(lib.photos.len(), 2 + (KEEP_RECENT - 1));
        assert!(lib.photo(ids[19]).is_some());
        assert!(lib.photo(ids[1]).is_none());
        assert!(deleted.contains(&original_key(ids[1])));
        // the version outlives its photo
        assert!(lib.version(v).is_some());
        assert_eq!(lib.remove_version(v), vec![version_key(v), version_thumb_key(v)]);
        assert!(lib.remove_version(v).is_empty());
    }

    #[test]
    fn budget_lets_go_of_oldest_unkept() {
        let mut lib = Library::default();
        let old = lib.add_photo("old.jpg", 1, 1, STORAGE_BUDGET, 1.0);
        let new = lib.add_photo("new.jpg", 1, 1, 10, 2.0);
        lib.prune(None);
        assert!(lib.photo(old).is_none() && lib.photo(new).is_some());
        // a kept photo is never let go, even over budget
        let big = lib.add_photo("big.jpg", 1, 1, u64::MAX, 3.0);
        lib.set_kept(big, true);
        lib.prune(None);
        assert!(lib.photo(big).is_some());
        assert_eq!(lib.stored_bytes(), u64::MAX);
    }
}
