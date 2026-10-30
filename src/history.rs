//! Notification history.
//!
//! Entries are newest first. The cap is a hard limit: a new notification
//! drops the oldest ones, and a lower cap drops them from disk on the next
//! load. Image pixel data is never stored. An icon is a glyph, a theme icon
//! name, or a file path.

use crate::model::{IconSource, Note, Urgency};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: u32,
    pub app_name: String,
    pub icon: HistoryIcon,
    pub summary: String,
    pub body: String,
    pub urgency: String,
    pub timestamp: i64,
    pub actions: Vec<HistoryAction>,
    pub dismissed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryAction {
    pub key: String,
    pub label: String,
}

/// Icon reference stored with a history entry. There is no pixel variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryIcon {
    pub kind: HistoryIconKind,
    pub value: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryIconKind {
    Glyph,
    IconName,
    File,
}

#[derive(Debug)]
pub struct HistoryLog {
    entries: Vec<HistoryEntry>,
    max: usize,
    persist: bool,
    path: PathBuf,
}

#[derive(Serialize)]
struct HistoryDocument<'a> {
    version: u32,
    entries: &'a [HistoryEntry],
}

#[derive(Deserialize)]
struct HistoryDocumentOwned {
    version: u32,
    entries: Vec<HistoryEntry>,
}

impl HistoryLog {
    /// In-memory log. Nothing is read or written.
    #[cfg(test)]
    pub fn memory(max: usize) -> Self {
        Self::load(max, false, PathBuf::new())
    }

    /// Load `$XDG_STATE_HOME/den-den-mushi/history.json` when `persist` is set.
    ///
    /// A missing file is an empty log. A file this process cannot decode is
    /// ignored so a bad write cannot stop the daemon. When persistence is
    /// off, an existing file is left untouched.
    pub fn load(max: usize, persist: bool, path: PathBuf) -> Self {
        let mut log = Self {
            entries: Vec::new(),
            max,
            persist,
            path,
        };
        if !persist {
            return log;
        }
        match read_document(&log.path) {
            Ok(entries) => {
                log.entries = entries;
                if log.normalize() {
                    log.store();
                } else {
                    tighten(&log.path);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("den-den-mushi: ignoring history file ({error})");
            }
        }
        log
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// JSON array, newest first. This is the `History` return value.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&self.entries).unwrap_or_else(|_| "[]".to_string())
    }

    /// Insert or replace `note` at the front. Returns whether the log changed.
    pub fn record(&mut self, note: &Note, actions: &[String], dismissed: bool) -> bool {
        if self.max == 0 {
            return self.clear();
        }
        let entry = HistoryEntry::from_note(note, actions, dismissed);
        self.entries.retain(|existing| existing.id != entry.id);
        self.entries.insert(0, entry);
        self.entries.truncate(self.max);
        true
    }

    /// User dismiss (freedesktop reason 2). Expiry and a client close do not.
    pub fn mark_dismissed(&mut self, id: u32) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        if entry.dismissed {
            return false;
        }
        entry.dismissed = true;
        true
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() != before
    }

    pub fn clear(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.entries.clear();
        true
    }

    pub fn store(&self) {
        if !self.persist {
            return;
        }
        let Some(bytes) = file_bytes(&self.entries) else {
            return;
        };
        if let Err(error) = write_atomic(&self.path, &bytes) {
            eprintln!("den-den-mushi: could not store history: {error}");
        }
    }

    /// Drop duplicate ids (keep the newest copy) and anything past the cap.
    fn normalize(&mut self) -> bool {
        let original = self.entries.clone();
        let mut seen = HashSet::new();
        self.entries.retain(|entry| seen.insert(entry.id));
        self.entries.truncate(self.max);
        self.entries != original
    }
}

impl HistoryEntry {
    fn from_note(note: &Note, actions: &[String], dismissed: bool) -> Self {
        Self {
            id: note.id,
            app_name: note.app_name.clone(),
            icon: history_icon(note),
            summary: note.summary.clone(),
            body: note.body.clone(),
            urgency: urgency_name(note.urgency).to_string(),
            timestamp: note.created_unix,
            actions: history_actions(actions),
            dismissed,
        }
    }
}

pub fn parse_entries(json: &str) -> Result<Vec<HistoryEntry>, String> {
    serde_json::from_str(json).map_err(|error| format!("history response was not a list ({error})"))
}

fn urgency_name(urgency: Urgency) -> &'static str {
    match urgency {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::Critical => "critical",
    }
}

/// Glyph, icon name, or file path. `image-data` pixels become the glyph.
pub fn history_icon(note: &Note) -> HistoryIcon {
    match &note.icon {
        IconSource::Glyph(value) if !value.is_empty() => HistoryIcon {
            kind: HistoryIconKind::Glyph,
            value: value.clone(),
        },
        IconSource::IconName(value) if !value.is_empty() => HistoryIcon {
            kind: HistoryIconKind::IconName,
            value: value.clone(),
        },
        IconSource::File(path) => HistoryIcon {
            kind: HistoryIconKind::File,
            value: path.display().to_string(),
        },
        IconSource::Rgba { .. } | IconSource::Glyph(_) | IconSource::IconName(_) => HistoryIcon {
            kind: HistoryIconKind::Glyph,
            value: note.glyph_fallback.clone(),
        },
    }
}

fn history_actions(actions: &[String]) -> Vec<HistoryAction> {
    let mut out = Vec::new();
    let mut index = 0;
    while index + 1 < actions.len() {
        let key = &actions[index];
        let label = &actions[index + 1];
        if !key.is_empty() {
            out.push(HistoryAction {
                key: key.clone(),
                label: label.clone(),
            });
        }
        index += 2;
    }
    out
}

fn file_bytes(entries: &[HistoryEntry]) -> Option<Vec<u8>> {
    let document = HistoryDocument {
        version: FILE_VERSION,
        entries,
    };
    let mut bytes = serde_json::to_vec_pretty(&document).ok()?;
    bytes.push(b'\n');
    Some(bytes)
}

fn read_document(path: &Path) -> io::Result<Vec<HistoryEntry>> {
    let text = fs::read_to_string(path)?;
    let document: HistoryDocumentOwned = serde_json::from_str(&text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if document.version != FILE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported history version {}", document.version),
        ));
    }
    Ok(document.entries)
}

/// Write `bytes` by renaming a temp file in the same directory.
///
/// Readers see the previous file or the new file. A crash during the write
/// leaves the previous file in place. The file mode is `0600` and the
/// directory mode is `0700`.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    prepare_parent(path)?;
    let tmp = temp_path(path);
    let write_result = write_temp(&tmp, bytes);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    fs::rename(&tmp, path)?;
    tighten(path);
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn write_temp(tmp: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn prepare_parent(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn tighten(path: &Path) {
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("history.json");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(".{name}.{}.{nanos}.tmp", std::process::id());
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
        _ => PathBuf::from(tmp_name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{IconSource, Note, Urgency, GLYPH_BELL};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            let n = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "den-den-mushi-history-{}-{nanos}-{n}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("scratch dir");
            Self { dir }
        }

        fn path(&self) -> PathBuf {
            self.dir.join("history.json")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn note(id: u32, summary: &str) -> Note {
        Note {
            id,
            app_name: "Spotify".into(),
            summary: summary.into(),
            body: format!("body {id}"),
            icon: IconSource::IconName("spotify".into()),
            glyph_fallback: GLYPH_BELL.into(),
            actions: vec![("pay".into(), "Pay".into())],
            has_default: true,
            urgency: Urgency::Normal,
            battery: false,
            expire_ms: None,
            exec: None,
            created_unix: 1_700_000_000 + i64::from(id),
        }
    }

    fn actions() -> Vec<String> {
        vec!["default".into(), "Open".into(), "pay".into(), "Pay".into()]
    }

    fn record_all(log: &mut HistoryLog, ids: &[u32]) {
        for id in ids {
            assert!(log.record(&note(*id, &format!("note {id}")), &actions(), false));
        }
    }

    #[test]
    fn newest_entry_is_first() {
        let mut log = HistoryLog::memory(100);
        record_all(&mut log, &[4, 7, 9]);
        let ids: Vec<u32> = log.entries().iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![9, 7, 4]);
        assert_eq!(log.entries()[0].summary, "note 9");
        assert_eq!(log.entries()[0].app_name, "Spotify");
        assert_eq!(log.entries()[0].urgency, "normal");
        assert_eq!(log.entries()[0].timestamp, 1_700_000_009);
        assert!(!log.entries()[0].dismissed);
        assert_eq!(
            log.entries()[0].actions,
            vec![
                HistoryAction {
                    key: "default".into(),
                    label: "Open".into(),
                },
                HistoryAction {
                    key: "pay".into(),
                    label: "Pay".into(),
                },
            ]
        );
        assert_eq!(log.entries()[0].icon.kind, HistoryIconKind::IconName);
        assert_eq!(log.entries()[0].icon.value, "spotify");
    }

    #[test]
    fn cap_drops_the_oldest_entries() {
        let mut log = HistoryLog::memory(2);
        record_all(&mut log, &[1, 2, 3]);
        let ids: Vec<u32> = log.entries().iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![3, 2]);
        assert!(log.entries().iter().all(|entry| entry.id != 1));
    }

    #[test]
    fn zero_cap_keeps_nothing() {
        let mut log = HistoryLog::memory(0);
        assert!(!log.record(&note(1, "nope"), &actions(), false));
        assert!(log.entries().is_empty());
    }

    #[test]
    fn replacing_an_id_moves_it_to_the_front() {
        let mut log = HistoryLog::memory(10);
        record_all(&mut log, &[1, 2]);
        assert!(log.mark_dismissed(1));
        let mut updated = note(1, "replaced");
        updated.body = "new body".into();
        assert!(log.record(&updated, &actions(), false));
        let ids: Vec<u32> = log.entries().iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(log.entries()[0].summary, "replaced");
        assert!(!log.entries()[0].dismissed);
        assert_eq!(log.entries().len(), 2);
    }

    #[test]
    fn clear_and_remove_drop_entries() {
        let mut log = HistoryLog::memory(10);
        record_all(&mut log, &[1, 2, 3]);
        assert!(log.remove(2));
        assert!(!log.remove(2));
        let ids: Vec<u32> = log.entries().iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![3, 1]);
        assert!(log.clear());
        assert!(log.entries().is_empty());
        assert!(!log.clear());
        assert!(!log.mark_dismissed(3));
    }

    #[test]
    fn dismiss_flag_sticks_until_the_entry_is_replaced() {
        let mut log = HistoryLog::memory(10);
        record_all(&mut log, &[5]);
        assert!(!log.mark_dismissed(9));
        assert!(log.mark_dismissed(5));
        assert!(!log.mark_dismissed(5));
        assert!(log.entries()[0].dismissed);
    }

    #[test]
    fn image_pixels_are_not_stored() {
        let mut pixels = note(8, "with image");
        pixels.icon = IconSource::Rgba {
            width: 1,
            height: 1,
            rgba: vec![9, 8, 7, 6],
        };
        pixels.glyph_fallback = GLYPH_BELL.into();
        let mut log = HistoryLog::memory(10);
        assert!(log.record(&pixels, &[], false));
        let json = log.to_json();
        assert!(!json.contains("\"rgba\""));
        assert!(!json.contains("[9,8,7,6]"));
        assert!(!json.contains("9,8,7,6"));
        assert_eq!(log.entries()[0].icon.kind, HistoryIconKind::Glyph);
        assert_eq!(log.entries()[0].icon.value, GLYPH_BELL);
        assert!(json.contains(GLYPH_BELL));
        let parsed = parse_entries(&json).unwrap();
        assert_eq!(parsed, log.entries());
    }

    #[test]
    fn file_icon_stores_the_path_only() {
        let mut image = note(3, "path");
        image.icon = IconSource::File(PathBuf::from("/tmp/album-art.png"));
        let mut log = HistoryLog::memory(10);
        log.record(&image, &[], false);
        assert_eq!(log.entries()[0].icon.kind, HistoryIconKind::File);
        assert_eq!(log.entries()[0].icon.value, "/tmp/album-art.png");
        assert!(!log.to_json().contains("rgba"));
    }

    #[test]
    fn persist_roundtrip_is_private_and_newest_first() {
        let scratch = Scratch::new();
        let path = scratch.path();
        let mut log = HistoryLog::load(2, true, path.clone());
        record_all(&mut log, &[1, 2, 3]);
        log.store();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(&scratch.dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        let names: Vec<_> = fs::read_dir(&scratch.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["history.json".to_string()]);

        let text = fs::read_to_string(&path).unwrap();
        let document: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(document["version"], 1);
        assert!(text.ends_with('\n'));
        assert!(!text.contains("rgba"));

        let loaded = HistoryLog::load(2, true, path);
        let ids: Vec<u32> = loaded.entries().iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec![3, 2]);
        assert_eq!(loaded.entries()[0].summary, "note 3");
        assert_eq!(loaded.entries()[1].body, "body 2");
    }

    #[test]
    fn a_lower_cap_rewrites_the_file_without_the_dropped_entries() {
        let scratch = Scratch::new();
        let path = scratch.path();
        let mut log = HistoryLog::load(10, true, path.clone());
        record_all(&mut log, &[1, 2, 3]);
        log.store();
        let loaded = HistoryLog::load(1, true, path.clone());
        assert_eq!(loaded.entries().len(), 1);
        assert_eq!(loaded.entries()[0].id, 3);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("note 3"));
        assert!(!text.contains("note 1"));
        assert!(!text.contains("note 2"));
    }

    #[test]
    fn loose_file_mode_is_tightened_when_the_log_is_loaded() {
        let scratch = Scratch::new();
        let path = scratch.path();
        fs::write(&path, "{\"version\":1,\"entries\":[]}\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let log = HistoryLog::load(10, true, path.clone());
        assert!(log.entries().is_empty());
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn replace_does_not_mix_the_previous_file_with_the_new_one() {
        let scratch = Scratch::new();
        let path = scratch.path();
        let mut log = HistoryLog::load(10, true, path.clone());
        record_all(&mut log, &[1]);
        log.store();
        let first = fs::read(&path).unwrap();
        record_all(&mut log, &[2]);
        log.store();
        let second = fs::read(&path).unwrap();
        assert_ne!(first, second);
        let text = String::from_utf8(second).unwrap();
        let document: serde_json::Value = serde_json::from_str(&text).unwrap();
        let ids: Vec<u64> = document["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids, vec![2, 1]);
    }

    #[test]
    fn memory_only_does_not_read_or_write_the_file() {
        let scratch = Scratch::new();
        let path = scratch.path();
        fs::write(&path, "{\"version\":1,\"entries\":[{\"id\":1}]}\n").unwrap();
        let before = fs::read(&path).unwrap();
        let mut log = HistoryLog::load(10, false, path.clone());
        assert!(log.entries().is_empty());
        assert!(log.record(&note(4, "ram"), &actions(), false));
        log.store();
        assert_eq!(log.entries().len(), 1);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_broken_file_is_ignored() {
        let scratch = Scratch::new();
        let path = scratch.path();
        fs::write(&path, b"{not json").unwrap();
        let log = HistoryLog::load(10, true, path.clone());
        assert!(log.entries().is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"{not json");
    }

    #[test]
    fn clear_persists_an_empty_list() {
        let scratch = Scratch::new();
        let path = scratch.path();
        let mut log = HistoryLog::load(10, true, path.clone());
        record_all(&mut log, &[1, 2]);
        log.store();
        assert!(log.clear());
        log.store();
        let loaded = HistoryLog::load(10, true, path);
        assert!(loaded.entries().is_empty());
    }
}
