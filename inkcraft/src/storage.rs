//! Binary `.ink` notebook files, debounced background autosave, and SQLite metadata.
//!
//! File layout (little-endian):
//!
//! | offset | size | field                          |
//! |--------|------|--------------------------------|
//! | 0      | 4    | magic `INK1`                   |
//! | 4      | 2    | format version (currently 1)   |
//! | 6      | 8    | payload length in bytes        |
//! | 14     | n    | bincode (standard config) body |
//!
//! Writes go to a sibling `.ink.tmp` file that is fsynced and then renamed over
//! the target, so a crash mid-save leaves the previous file intact.

use crate::model::{Notebook, PageImage, Pattern, Stroke};
use serde::Deserialize;
use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use rusqlite::{params, Connection};
use std::{
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAGIC: [u8; 4] = *b"INK1";
pub const FORMAT_VERSION: u16 = 4;
/// Older versions still load: v1 had no page images, v2 had no pattern,
/// v3 had no guides.
const V1: u16 = 1;
const V2: u16 = 2;
const V3: u16 = 3;
const HEADER_LEN: usize = 4 + 2 + 8;

/// Quiet period after the last edit before an autosave is written.
pub const DEBOUNCE: Duration = Duration::from_millis(500);
/// Upper bound on how long continuous edits can defer a save.
pub const MAX_SAVE_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum StorageError {
    Io(io::Error),
    Encode(bincode::error::EncodeError),
    Decode(bincode::error::DecodeError),
    Sql(rusqlite::Error),
    BadMagic,
    UnsupportedVersion(u16),
    Truncated,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Encode(e) => write!(f, "encode error: {e}"),
            Self::Decode(e) => write!(f, "decode error: {e}"),
            Self::Sql(e) => write!(f, "metadata database error: {e}"),
            Self::BadMagic => write!(f, "not an InkCraft notebook (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported notebook format version {v}"),
            Self::Truncated => write!(f, "notebook file is truncated"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<bincode::error::EncodeError> for StorageError {
    fn from(e: bincode::error::EncodeError) -> Self {
        Self::Encode(e)
    }
}

impl From<bincode::error::DecodeError> for StorageError {
    fn from(e: bincode::error::DecodeError) -> Self {
        Self::Decode(e)
    }
}

impl From<rusqlite::Error> for StorageError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

pub fn encode_notebook(notebook: &Notebook) -> Result<Vec<u8>, StorageError> {
    let payload = bincode::serde::encode_to_vec(notebook, bincode::config::standard())?;
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// The version 1 layout, kept only to read old files. Field order must match.
#[derive(Deserialize)]
struct NotebookV1 {
    title: String,
    strokes: Vec<Stroke>,
}

/// The version 2 layout: page images, no pattern.
#[derive(Deserialize)]
struct NotebookV2 {
    title: String,
    strokes: Vec<Stroke>,
    images: Vec<PageImage>,
}

/// The version 3 layout: page images and pattern, no guides.
#[derive(Deserialize)]
struct NotebookV3 {
    title: String,
    strokes: Vec<Stroke>,
    images: Vec<PageImage>,
    pattern: Pattern,
}

pub fn decode_notebook(bytes: &[u8]) -> Result<Notebook, StorageError> {
    if bytes.len() < HEADER_LEN {
        return Err(StorageError::Truncated);
    }
    if bytes[..4] != MAGIC {
        return Err(StorageError::BadMagic);
    }
    let version = u16::from_le_bytes([bytes[4], bytes[5]]);
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&bytes[6..14]);
    let len = u64::from_le_bytes(len_bytes) as usize;
    let end = HEADER_LEN.checked_add(len).ok_or(StorageError::Truncated)?;
    let payload = bytes.get(HEADER_LEN..end).ok_or(StorageError::Truncated)?;
    let config = bincode::config::standard();
    match version {
        V1 => {
            let (old, _): (NotebookV1, _) = bincode::serde::decode_from_slice(payload, config)?;
            Ok(Notebook {
                title: old.title,
                strokes: old.strokes,
                ..Default::default()
            })
        }
        V2 => {
            let (old, _): (NotebookV2, _) = bincode::serde::decode_from_slice(payload, config)?;
            Ok(Notebook {
                title: old.title,
                strokes: old.strokes,
                images: old.images,
                ..Default::default()
            })
        }
        V3 => {
            let (old, _): (NotebookV3, _) = bincode::serde::decode_from_slice(payload, config)?;
            Ok(Notebook {
                title: old.title,
                strokes: old.strokes,
                images: old.images,
                pattern: old.pattern,
                guides: Vec::new(),
            })
        }
        FORMAT_VERSION => {
            let (notebook, _) = bincode::serde::decode_from_slice(payload, config)?;
            Ok(notebook)
        }
        other => Err(StorageError::UnsupportedVersion(other)),
    }
}

/// Writes the notebook atomically: temp file, fsync, rename.
pub fn save_notebook(path: &Path, notebook: &Notebook) -> Result<(), StorageError> {
    let bytes = encode_notebook(notebook)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("ink.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load_notebook(path: &Path) -> Result<Notebook, StorageError> {
    decode_notebook(&fs::read(path)?)
}

enum SaveMessage {
    Save { path: PathBuf, notebook: Notebook },
    Shutdown,
}

/// Background autosave worker with trailing-edge debounce.
///
/// Each save request replaces any pending one. The write happens once no new
/// request has arrived for [`DEBOUNCE`], or [`MAX_SAVE_DELAY`] after the first
/// pending request, whichever comes first. Dropping the handle writes any
/// pending save and joins the worker.
pub struct Autosaver {
    tx: Sender<SaveMessage>,
    worker: Option<JoinHandle<()>>,
    saved: Arc<Mutex<Option<SystemTime>>>,
}

impl Autosaver {
    pub fn spawn() -> Self {
        let (tx, rx) = unbounded();
        let saved = Arc::new(Mutex::new(None));
        let worker_saved = saved.clone();
        let worker = thread::Builder::new()
            .name("inkcraft-autosave".into())
            .spawn(move || autosave_loop(rx, worker_saved))
            .expect("failed to spawn autosave thread");
        Self {
            tx,
            worker: Some(worker),
            saved,
        }
    }

    pub fn request_save(&self, path: PathBuf, notebook: Notebook) {
        let _ = self.tx.send(SaveMessage::Save { path, notebook });
    }

    /// Time of the most recent successful write, if any.
    pub fn last_saved(&self) -> Option<SystemTime> {
        self.saved.lock().ok().and_then(|guard| *guard)
    }
}

impl Drop for Autosaver {
    fn drop(&mut self) {
        let _ = self.tx.send(SaveMessage::Shutdown);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

fn autosave_loop(rx: Receiver<SaveMessage>, saved: Arc<Mutex<Option<SystemTime>>>) {
    while let Ok(message) = rx.recv() {
        let (mut path, mut notebook) = match message {
            SaveMessage::Save { path, notebook } => (path, notebook),
            SaveMessage::Shutdown => return,
        };
        let started = Instant::now();
        let mut shutting_down = false;
        loop {
            let remaining = MAX_SAVE_DELAY.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match rx.recv_timeout(DEBOUNCE.min(remaining)) {
                Ok(SaveMessage::Save { path: p, notebook: nb }) => {
                    path = p;
                    notebook = nb;
                }
                Ok(SaveMessage::Shutdown) => {
                    shutting_down = true;
                    break;
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    shutting_down = true;
                    break;
                }
            }
        }
        match save_notebook(&path, &notebook) {
            Ok(()) => {
                if let Ok(mut guard) = saved.lock() {
                    *guard = Some(SystemTime::now());
                }
            }
            Err(e) => eprintln!("autosave failed for {}: {e}", path.display()),
        }
        if shutting_down {
            return;
        }
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS folders (
    id        INTEGER PRIMARY KEY,
    parent_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
    name      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notebooks (
    id          INTEGER PRIMARY KEY,
    folder_id   INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    title       TEXT NOT NULL,
    path        TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL,
    modified_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS tags (
    notebook_id INTEGER NOT NULL REFERENCES notebooks(id) ON DELETE CASCADE,
    tag         TEXT NOT NULL,
    PRIMARY KEY (notebook_id, tag)
);
CREATE INDEX IF NOT EXISTS idx_tags_tag ON tags(tag);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotebookRecord {
    pub id: i64,
    pub title: String,
    pub path: String,
    pub modified_at: i64,
}

/// SQLite-backed notebook hierarchy and tag index.
pub struct Metadata {
    conn: Connection,
}

impl Metadata {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, StorageError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StorageError> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn create_folder(&self, name: &str, parent: Option<i64>) -> Result<i64, StorageError> {
        self.conn.execute(
            "INSERT INTO folders (name, parent_id) VALUES (?1, ?2)",
            params![name, parent],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Inserts or updates the row for `path` and returns its id.
    pub fn register_notebook(
        &self,
        title: &str,
        path: &Path,
        folder: Option<i64>,
        now: i64,
    ) -> Result<i64, StorageError> {
        let id = self.conn.query_row(
            "INSERT INTO notebooks (folder_id, title, path, created_at, modified_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(path) DO UPDATE SET title = excluded.title
             RETURNING id",
            params![folder, title, path.to_string_lossy(), now],
            |row| row.get(0),
        )?;
        Ok(id)
    }

    pub fn touch_notebook(&self, path: &Path, now: i64) -> Result<(), StorageError> {
        self.conn.execute(
            "UPDATE notebooks SET modified_at = ?2 WHERE path = ?1",
            params![path.to_string_lossy(), now],
        )?;
        Ok(())
    }

    pub fn add_tag(&self, notebook_id: i64, tag: &str) -> Result<(), StorageError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO tags (notebook_id, tag) VALUES (?1, ?2)",
            params![notebook_id, tag],
        )?;
        Ok(())
    }

    /// Notebooks in `folder` (`None` means the root), most recently modified first.
    pub fn notebooks_in_folder(&self, folder: Option<i64>) -> Result<Vec<NotebookRecord>, StorageError> {
        self.query_notebooks(
            "SELECT id, title, path, modified_at FROM notebooks
             WHERE folder_id IS ?1 ORDER BY modified_at DESC",
            params![folder],
        )
    }

    pub fn notebooks_with_tag(&self, tag: &str) -> Result<Vec<NotebookRecord>, StorageError> {
        self.query_notebooks(
            "SELECT n.id, n.title, n.path, n.modified_at FROM notebooks n
             JOIN tags t ON t.notebook_id = n.id
             WHERE t.tag = ?1 ORDER BY n.modified_at DESC",
            params![tag],
        )
    }

    fn query_notebooks(
        &self,
        sql: &str,
        args: impl rusqlite::Params,
    ) -> Result<Vec<NotebookRecord>, StorageError> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(args, |row| {
            Ok(NotebookRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                path: row.get(2)?,
                modified_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// Current Unix time in seconds.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Directory for notebooks and the metadata database. Honours `INKCRAFT_HOME`.
pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("INKCRAFT_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".inkcraft")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Stroke, StrokePoint};

    fn sample_notebook(strokes: usize) -> Notebook {
        Notebook {
            title: "Test".into(),
            strokes: (0..strokes)
                .map(|i| Stroke {
                    color: [10, 20, 30, 255],
                    width: 2.5,
                    points: (0..50)
                        .map(|j| StrokePoint {
                            x: i as f32 * 3.0 + j as f32,
                            y: j as f32 * 0.5,
                            pressure: 0.5,
                        })
                        .collect(),
                })
                .collect(),
            images: Vec::new(),
            pattern: crate::model::Pattern::Blank,
            guides: Vec::new(),
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("inkcraft-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn roundtrip_preserves_document() {
        let nb = sample_notebook(200);
        let bytes = encode_notebook(&nb).unwrap();
        assert_eq!(&bytes[..4], b"INK1");
        assert_eq!(decode_notebook(&bytes).unwrap(), nb);
    }

    #[test]
    fn loads_version_one_files_without_images() {
        #[derive(serde::Serialize)]
        struct V1 {
            title: String,
            strokes: Vec<Stroke>,
        }
        let nb = sample_notebook(2);
        let payload = bincode::serde::encode_to_vec(
            &V1 { title: nb.title.clone(), strokes: nb.strokes.clone() },
            bincode::config::standard(),
        )
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);

        let loaded = decode_notebook(&bytes).unwrap();
        assert_eq!(loaded.title, "Test");
        assert_eq!(loaded.strokes, nb.strokes);
        assert!(loaded.images.is_empty());
    }

    #[test]
    fn roundtrip_keeps_page_images() {
        let mut nb = sample_notebook(1);
        nb.images.push(crate::model::PageImage {
            rect: crate::model::Rect::new([0.0, 0.0], [612.0, 792.0]),
            data: vec![0x89, b'P', b'N', b'G', 1, 2, 3],
        });
        nb.pattern = crate::model::Pattern::Ruled;
        nb.guides.push(crate::model::Guide { vertical: true, pos: 42.5 });
        let decoded = decode_notebook(&encode_notebook(&nb).unwrap()).unwrap();
        assert_eq!(decoded, nb);
    }

    #[test]
    fn loads_version_three_files_without_guides() {
        #[derive(serde::Serialize)]
        struct V3 {
            title: String,
            strokes: Vec<Stroke>,
            images: Vec<PageImage>,
            pattern: Pattern,
        }
        let payload = bincode::serde::encode_to_vec(
            &V3 {
                title: "Old".into(),
                strokes: Vec::new(),
                images: Vec::new(),
                pattern: Pattern::Grid,
            },
            bincode::config::standard(),
        )
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&payload);

        let loaded = decode_notebook(&bytes).unwrap();
        assert_eq!(loaded.pattern, Pattern::Grid);
        assert!(loaded.guides.is_empty());
    }

    #[test]
    fn rejects_bad_magic_version_and_truncation() {
        let mut bytes = encode_notebook(&sample_notebook(1)).unwrap();
        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(decode_notebook(&bad_magic), Err(StorageError::BadMagic)));

        bytes[4] = 9;
        assert!(matches!(
            decode_notebook(&bytes),
            Err(StorageError::UnsupportedVersion(9))
        ));

        let good = encode_notebook(&sample_notebook(1)).unwrap();
        assert!(matches!(
            decode_notebook(&good[..good.len() - 1]),
            Err(StorageError::Truncated)
        ));
        assert!(matches!(decode_notebook(&[1, 2]), Err(StorageError::Truncated)));
    }

    #[test]
    fn save_then_load_from_disk() {
        let path = temp_path("roundtrip.ink");
        let nb = sample_notebook(5);
        save_notebook(&path, &nb).unwrap();
        assert!(!path.with_extension("ink.tmp").exists());
        assert_eq!(load_notebook(&path).unwrap(), nb);
    }

    #[test]
    fn autosave_writes_latest_state_after_burst() {
        let path = temp_path("autosave.ink");
        let autosaver = Autosaver::spawn();
        for n in 1..=20 {
            autosaver.request_save(path.clone(), sample_notebook(n));
        }
        drop(autosaver); // Drop flushes the pending save and joins the worker.
        assert_eq!(load_notebook(&path).unwrap(), sample_notebook(20));
    }

    #[test]
    fn metadata_hierarchy_and_tags() {
        let db = Metadata::open_in_memory().unwrap();
        let work = db.create_folder("Work", None).unwrap();
        let sub = db.create_folder("Site surveys", Some(work)).unwrap();
        let p1 = Path::new("/notes/a.ink");
        let p2 = Path::new("/notes/b.ink");
        let a = db.register_notebook("Floor plan", p1, Some(sub), 100).unwrap();
        let b = db.register_notebook("Ideas", p2, None, 200).unwrap();

        // Re-registering the same path is an upsert, not a duplicate row.
        let a_again = db.register_notebook("Floor plan v2", p1, Some(sub), 300).unwrap();
        assert_eq!(a, a_again);

        db.add_tag(a, "site").unwrap();
        db.add_tag(a, "site").unwrap();
        db.add_tag(b, "site").unwrap();

        // Upsert refreshes the title but leaves modified_at alone; only real edits bump it.
        let tagged = db.notebooks_with_tag("site").unwrap();
        assert_eq!(tagged.len(), 2);
        assert!(tagged.iter().any(|r| r.title == "Floor plan v2"));
        assert_eq!(tagged[0].title, "Ideas", "most recently modified first");

        assert_eq!(db.notebooks_in_folder(Some(sub)).unwrap().len(), 1);
        assert_eq!(db.notebooks_in_folder(None).unwrap()[0].title, "Ideas");

        db.touch_notebook(p2, 999).unwrap();
        assert_eq!(db.notebooks_in_folder(None).unwrap()[0].modified_at, 999);
    }
}
