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

impl PaneLayoutNode {
    /// Return a filtered copy keeping only pinned tabs. Returns None if no
    /// pinned tabs remain in this subtree.
    pub fn pinned_only(&self) -> Option<Self> {
        match self {
            PaneLayoutNode::Pane(pane) => {
                let pinned_tabs: Vec<_> = pane.tabs.iter()
                    .filter(|t| t.pinned)
                    .cloned()
                    .collect();
                if pinned_tabs.is_empty() {
                    None
                } else {
                    let active_tab = pinned_tabs.len().saturating_sub(1).min(pane.active_tab);
                    Some(PaneLayoutNode::Pane(PaneSession {
                        tabs: pinned_tabs,
                        active_tab,
                    }))
                }
            }
            PaneLayoutNode::Split { axis, ratio, a, b } => {
                match (a.pinned_only(), b.pinned_only()) {
                    (Some(a), Some(b)) => Some(PaneLayoutNode::Split {
                        axis: *axis,
                        ratio: *ratio,
                        a: Box::new(a),
                        b: Box::new(b),
                    }),
                    (Some(node), None) | (None, Some(node)) => Some(node),
                    (None, None) => None,
                }
            }
        }
    }

    /// Recursively collect all TabSessions from the tree.
    pub fn collect_tabs(&self) -> Vec<&TabSession> {
        match self {
            PaneLayoutNode::Pane(pane) => pane.tabs.iter().collect(),
            PaneLayoutNode::Split { a, b, .. } => {
                let mut tabs = a.collect_tabs();
                tabs.extend(b.collect_tabs());
                tabs
            }
        }
    }
}

impl SessionState {
    /// Count total tabs across all windows.
    pub fn tab_count(&self) -> usize {
        self.windows.iter()
            .map(|w| w.pane_layout.collect_tabs().len())
            .sum()
    }

    /// Check if any tab in the session is pinned.
    pub fn has_pinned(&self) -> bool {
        self.windows.iter()
            .any(|w| w.pane_layout.collect_tabs().iter().any(|t| t.pinned))
    }

    /// Check if ALL tabs are pinned (no ephemeral tabs).
    pub fn all_pinned(&self) -> bool {
        self.all_tabs().iter().all(|t| t.pinned)
    }

    /// Collect all tabs across all windows (flattened).
    pub fn all_tabs(&self) -> Vec<&TabSession> {
        self.windows.iter()
            .flat_map(|w| w.pane_layout.collect_tabs())
            .collect()
    }

    /// Return a copy containing only pinned tabs. Returns None if no pinned
    /// tabs exist in any window.
    pub fn pinned_only(&self) -> Option<Self> {
        let windows: Vec<_> = self.windows.iter()
            .filter_map(|w| {
                w.pane_layout.pinned_only().map(|layout| WindowSession {
                    is_main: w.is_main,
                    pane_layout: layout,
                })
            })
            .collect();
        if windows.is_empty() {
            None
        } else {
            Some(SessionState {
                session_id: self.session_id,
                pid: self.pid,
                windows,
            })
        }
    }
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

/// Check if the lock file's PID is still alive (and not a zombie).
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
    is_pid_alive(pid)
}

/// Check if a PID is alive and not a zombie process.
fn is_pid_alive(pid: i32) -> bool {
    // kill(pid, 0) checks if process exists without sending a signal
    if unsafe { libc::kill(pid, 0) != 0 } {
        return false;
    }
    // Zombie processes pass kill(pid, 0) but can't clean up their session
    // files. Check /proc/<pid>/status to detect them.
    let status_path = format!("/proc/{pid}/status");
    if let Ok(status) = fs::read_to_string(&status_path) {
        for line in status.lines() {
            if let Some(state) = line.strip_prefix("State:") {
                return !state.trim_start().starts_with('Z');
            }
        }
    }
    false
}

/// Check if any other cosmic-term process has an active session lock.
/// Used to determine if this is the first launch (restore) vs. additional window (fresh).
pub fn any_other_instance_running() -> bool {
    let Some(dir) = session_dir() else {
        return false;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return false;
    };
    let my_pid = std::process::id() as i32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "lock") {
            if let Ok(contents) = fs::read_to_string(&path) {
                if let Ok(pid) = contents.trim().parse::<i32>() {
                    if pid != my_pid && is_pid_alive(pid) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Find orphaned sessions (session files whose lock PIDs are no longer alive).
/// Also cleans up stale lock files that have no corresponding JSON.
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
        } else if path.extension().is_some_and(|e| e == "lock") {
            // Clean up stale lock files with no corresponding JSON
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if let Ok(session_id) = stem.parse::<u64>() {
                let json_path = dir.join(format!("{session_id}.json"));
                if !json_path.exists() && !is_lock_active(session_id) {
                    let _ = fs::remove_file(&path);
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

/// Delete backup files for a session EXCEPT those in the keep set.
pub fn cleanup_backups_except(session_id: u64, keep: &std::collections::HashSet<String>) {
    if let Some(dir) = backup_dir() {
        let prefix = format!("{session_id}_");
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with(&prefix) && !keep.contains(name) {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        }
    }
}

/// Delete all session files, lock files, and backup files.
pub fn cleanup_all_sessions() {
    if let Some(dir) = session_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    if let Some(dir) = backup_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
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

/// Decode the session start time from session_id and pid.
/// Reverses the XOR encoding from generate_session_id().
pub fn decode_start_time(session_id: u64, pid: u32) -> Option<SystemTime> {
    let ts_ms = session_id ^ ((pid as u64) << 32);
    // Sanity check: timestamp should be reasonable (after 2020, before 2100)
    if ts_ms > 1_577_836_800_000 && ts_ms < 4_102_444_800_000 {
        Some(UNIX_EPOCH + std::time::Duration::from_millis(ts_ms))
    } else {
        None
    }
}

/// Get session end time from the JSON file's modification time.
pub fn get_end_time(session_id: u64) -> Option<SystemTime> {
    let dir = session_dir()?;
    let path = dir.join(format!("{session_id}.json"));
    fs::metadata(&path).ok()?.modified().ok()
}

/// Scan scrollback text for the last `claude --resume <id>` command.
pub fn find_claude_resume_id(scrollback_text: &str) -> Option<&str> {
    for line in scrollback_text.lines().rev() {
        if let Some(pos) = line.find("claude --resume ") {
            let after = &line[pos + "claude --resume ".len()..];
            let token: &str = after.split_whitespace().next()?;
            if token.len() >= 8 && token.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
                return Some(token);
            }
        }
    }
    None
}

/// Format a unix timestamp as local time "YYYY-MM-DD HH:MM:SS".
pub fn format_timestamp(unix_secs: u64) -> String {
    let time_t = unix_secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&time_t, &mut tm) };
    let mut buf = [0u8; 64];
    let fmt = std::ffi::CString::new("%Y-%m-%d %H:%M:%S").unwrap();
    let len = unsafe {
        libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), fmt.as_ptr(), &tm)
    };
    String::from_utf8_lossy(&buf[..len]).to_string()
}
