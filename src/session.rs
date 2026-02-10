use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Top-level session state persisted to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub session_id: u64,
    pub pid: u32,
    pub windows: Vec<WindowSession>,
}

/// One window's saved state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowSession {
    pub is_main: bool,
    pub pane_layout: PaneLayoutNode,
}

/// Serializable pane tree mirroring pane_grid::Node / Configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PaneLayoutNode {
    Split {
        axis: SplitAxis,
        ratio: f32,
        a: Box<PaneLayoutNode>,
        b: Box<PaneLayoutNode>,
    },
    Pane(PaneSession),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

/// One pane containing tabs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaneSession {
    pub tabs: Vec<TabSession>,
    pub active_tab: usize,
}

/// One terminal tab's saved state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TabSession {
    pub profile_id: Option<u64>,
    pub tab_title: String,
    pub tab_title_override: Option<String>,
    pub working_directory: Option<String>,
    pub zoom_adj: i8,
    pub pinned: bool,
    /// Filename (not full path) within the backups directory.
    pub scrollback_file: Option<String>,
}

/// Maximum scrollback text to save per terminal (100 KB).
const MAX_SCROLLBACK_BYTES: usize = 100 * 1024;

fn cache_base() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("cosmic-term"))
}

pub fn session_dir() -> Option<PathBuf> {
    cache_base().map(|d| d.join("sessions"))
}

pub fn backup_dir() -> Option<PathBuf> {
    cache_base().map(|d| d.join("backups"))
}

pub fn generate_session_id() -> u64 {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let pid = std::process::id() as u64;
    // Simple combination — unique enough for single-machine use
    ts ^ (pid << 32)
}

/// Atomically write session state to JSON.
pub fn write_session(state: &SessionState) -> Option<()> {
    let dir = session_dir()?;
    fs::create_dir_all(&dir).ok()?;

    let path = dir.join(format!("{}.json", state.session_id));
    let tmp_path = dir.join(format!("{}.json.tmp", state.session_id));

    let json = serde_json::to_string_pretty(state).ok()?;
    let mut f = fs::File::create(&tmp_path).ok()?;
    f.write_all(json.as_bytes()).ok()?;
    f.sync_all().ok()?;
    fs::rename(&tmp_path, &path).ok()?;
    Some(())
}

/// Read a session state from disk.
pub fn read_session(session_id: u64) -> Option<SessionState> {
    let dir = session_dir()?;
    let path = dir.join(format!("{session_id}.json"));
    let data = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Write a PID lock file.
pub fn write_lock(session_id: u64) -> Option<()> {
    let dir = session_dir()?;
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{session_id}.lock"));
    fs::write(&path, std::process::id().to_string()).ok()
}

/// Remove a lock file.
pub fn remove_lock(session_id: u64) -> Option<()> {
    let dir = session_dir()?;
    let path = dir.join(format!("{session_id}.lock"));
    let _ = fs::remove_file(&path);
    Some(())
}

/// Check if the lock file's PID is still alive.
pub fn is_lock_active(session_id: u64) -> bool {
    let Some(dir) = session_dir() else {
        return false;
    };
    let path = dir.join(format!("{session_id}.lock"));
    let Ok(contents) = fs::read_to_string(&path) else {
        return false;
    };
    let Ok(pid) = contents.trim().parse::<i32>() else {
        return false;
    };
    // kill(pid, 0) checks if process exists without sending a signal
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Find orphaned sessions (session files whose lock PIDs are no longer alive).
pub fn find_orphaned_sessions() -> Vec<SessionState> {
    let Some(dir) = session_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Ok(session_id) = stem.parse::<u64>() {
                if !is_lock_active(session_id) {
                    if let Some(state) = read_session(session_id) {
                        sessions.push(state);
                    }
                }
            }
        }
    }
    sessions
}

/// Delete a session's JSON, lock, and all backup files.
pub fn cleanup_session(session_id: u64) {
    cleanup_session_meta(session_id);
    cleanup_backups(session_id);
}

/// Delete only the session JSON and lock file (not scrollback backups).
pub fn cleanup_session_meta(session_id: u64) {
    if let Some(dir) = session_dir() {
        let _ = fs::remove_file(dir.join(format!("{session_id}.json")));
        let _ = fs::remove_file(dir.join(format!("{session_id}.lock")));
    }
}

pub fn cleanup_backups(session_id: u64) {
    if let Some(dir) = backup_dir() {
        let prefix = format!("{session_id}_");
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&prefix) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

/// Save scrollback text for one tab, capped at MAX_SCROLLBACK_BYTES.
pub fn save_scrollback(session_id: u64, tab_idx: usize, content: &str) -> Option<String> {
    let dir = backup_dir()?;
    fs::create_dir_all(&dir).ok()?;

    let filename = format!("{session_id}_{tab_idx}.txt");
    let path = dir.join(&filename);

    // Truncate to last MAX_SCROLLBACK_BYTES bytes
    let bytes = content.as_bytes();
    let start = bytes.len().saturating_sub(MAX_SCROLLBACK_BYTES);
    let truncated = &bytes[start..];
    // Find first newline to avoid cutting mid-line
    let offset = if start > 0 {
        truncated.iter().position(|&b| b == b'\n').map_or(0, |p| p + 1)
    } else {
        0
    };

    fs::write(&path, &truncated[offset..]).ok()?;
    Some(filename)
}

/// Get the path to a scrollback backup file.
pub fn scrollback_path(filename: &str) -> Option<PathBuf> {
    backup_dir().map(|d| d.join(filename))
}
