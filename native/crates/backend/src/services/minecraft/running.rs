use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize)]
pub struct RunningInfo {
    pub instance_id: String,
    pub started_at: u64,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogLine {
    pub line: String,
    pub stream: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ProcessStats {
    /// Percentage of a single CPU core (matches Task Manager/Activity
    /// Monitor's own per-process convention — can exceed 100% on a
    /// multi-threaded process using more than one core).
    pub cpu_percent: f32,
    pub memory_mb: u64,
}

/// How the most recent session of an instance ended — what the crash
/// helper starts from.
#[derive(Debug, Clone, Serialize)]
pub struct SessionEnd {
    pub code: Option<i32>,
    /// Exited abnormally (non-zero code, or Minecraft's own "Game crashed!"
    /// marker) and not because the player pressed Stop.
    pub crashed: bool,
    pub started_at: u64,
    pub pid: Option<u32>,
}

/// Minecraft prints one of these when its crash handler runs, whatever the
/// exit code ends up being.
const CRASH_MARKERS: &[&str] = &["#@!@# Game crashed!", "---- Minecraft Crash Report ----"];

/// How many lines of a single session's console output are kept — generous
/// enough to hold a real crash trace (which can run to hundreds of lines
/// once mods are involved) without letting a long play session grow the
/// buffer unbounded.
const MAX_LOG_LINES: usize = 5000;

/// Tracks which instances currently have a live game process, purely
/// in-memory — this is presence info for the UI ("Minecraft is running"),
/// not persisted state, so a launcher restart while a game happens to be
/// running just means it's no longer tracked (the game itself is
/// unaffected either way). Also buffers each instance's most recent
/// session's console output, so a crash can still be inspected afterward
/// even if the console panel wasn't open while it happened — the buffer is
/// replaced (not appended to) on every new launch.
#[derive(Clone, Default)]
pub struct RunningInstances {
    map: Arc<RwLock<HashMap<String, RunningInfo>>>,
    logs: Arc<RwLock<HashMap<String, Vec<LogLine>>>>,
    /// One long-lived `System`, reused across every poll — sysinfo computes
    /// CPU usage as a delta since that process was last refreshed on this
    /// same instance, so a fresh `System` per call would always read ~0%.
    sys: Arc<RwLock<sysinfo::System>>,
    last_exit: Arc<RwLock<HashMap<String, SessionEnd>>>,
    /// Instances the player force-stopped, so that exit isn't reported as a
    /// crash.
    killed: Arc<RwLock<HashSet<String>>>,
}

impl RunningInstances {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn mark_started(&self, instance_id: &str, started_at: u64, pid: Option<u32>) {
        self.map.write().await.insert(
            instance_id.to_string(),
            RunningInfo {
                instance_id: instance_id.to_string(),
                started_at,
                pid,
            },
        );
        self.logs.write().await.insert(instance_id.to_string(), Vec::new());
        self.killed.write().await.remove(instance_id);
    }

    pub async fn mark_killed(&self, instance_id: &str) {
        self.killed.write().await.insert(instance_id.to_string());
    }

    /// Records how a session ended and returns it. Called once, when the
    /// game process exits.
    pub async fn record_exit(&self, instance_id: &str, started_at: u64, pid: Option<u32>, code: Option<i32>) -> SessionEnd {
        let killed = self.killed.write().await.remove(instance_id);
        let crash_marker = self
            .logs
            .read()
            .await
            .get(instance_id)
            .is_some_and(|lines| lines.iter().any(|l| CRASH_MARKERS.iter().any(|m| l.line.contains(m))));
        let end = SessionEnd {
            code,
            crashed: !killed && (code.is_some_and(|c| c != 0) || crash_marker),
            started_at,
            pid,
        };
        self.last_exit.write().await.insert(instance_id.to_string(), end.clone());
        end
    }

    /// How this instance's last session (this app run) ended, if it has
    /// ended.
    pub async fn last_exit(&self, instance_id: &str) -> Option<SessionEnd> {
        self.last_exit.read().await.get(instance_id).cloned()
    }

    pub async fn mark_stopped(&self, instance_id: &str) -> Option<RunningInfo> {
        self.map.write().await.remove(instance_id)
    }

    pub async fn get(&self, instance_id: &str) -> Option<RunningInfo> {
        self.map.read().await.get(instance_id).cloned()
    }

    pub async fn list(&self) -> Vec<RunningInfo> {
        self.map.read().await.values().cloned().collect()
    }

    pub async fn push_log(&self, instance_id: &str, line: String, stream: &str) {
        let mut logs = self.logs.write().await;
        let buf = logs.entry(instance_id.to_string()).or_default();
        buf.push(LogLine {
            line,
            stream: stream.to_string(),
        });
        if buf.len() > MAX_LOG_LINES {
            let excess = buf.len() - MAX_LOG_LINES;
            buf.drain(0..excess);
        }
    }

    /// The buffered console output from this instance's most recent
    /// session (running or already exited) — empty if it has never been
    /// launched this app session.
    pub async fn get_log(&self, instance_id: &str) -> Vec<LogLine> {
        self.logs.read().await.get(instance_id).cloned().unwrap_or_default()
    }

    /// Live CPU/RAM usage of this instance's game process, or `None` if
    /// it isn't running (or its PID couldn't be determined).
    pub async fn process_stats(&self, instance_id: &str) -> Option<ProcessStats> {
        let pid = self.map.read().await.get(instance_id)?.pid?;
        let sys_pid = sysinfo::Pid::from_u32(pid);
        let mut sys = self.sys.write().await;
        sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);
        let process = sys.process(sys_pid)?;
        Some(ProcessStats {
            cpu_percent: process.cpu_usage(),
            memory_mb: process.memory() / (1024 * 1024),
        })
    }
}

/// Force-kills a process by PID. Used for "quit" from the UI when the
/// normal in-game quit isn't (or can't be) used. Best-effort: returns
/// whether the process was found and a kill signal was sent, not whether it
/// has actually exited yet.
pub fn kill_pid(pid: u32) -> bool {
    let mut sys = sysinfo::System::new();
    let sys_pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[sys_pid]), true);
    sys.process(sys_pid).map(|p| p.kill()).unwrap_or(false)
}
