use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use portable_pty::{native_pty_system, CommandBuilder, Child, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, broadcast};

use crate::error::Error;

/// Shells that are allowed for the `shell` field in SpawnRequest.
const ALLOWED_SHELLS: &[&str] = &[
    "/bin/sh",
    "/bin/bash",
    "/bin/zsh",
    "/bin/fish",
    "/bin/dash",
    "/bin/csh",
    "/bin/tcsh",
    "/bin/ksh",
    "/usr/bin/bash",
    "/usr/bin/zsh",
    "/usr/bin/fish",
    "/usr/local/bin/bash",
    "/usr/local/bin/zsh",
    "/usr/local/bin/fish",
    "/opt/homebrew/bin/bash",
    "/opt/homebrew/bin/zsh",
    "/opt/homebrew/bin/fish",
];

/// Maximum length for shell name.
const MAX_NAME_LEN: usize = 128;

static COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    format!("{n}-{ts:x}-{:x}", std::process::id())
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellInfo {
    pub id: String,
    pub name: String,
    pub shell_type: ShellType,
    pub status: ShellStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellType {
    Local,
    Remote,
    Agent,
    Command,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellStatus {
    Running,
    Dead,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SpawnRequest {
    pub name: Option<String>,
    pub shell: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    pub agent: Option<String>,
    pub cmd: Option<String>,
}

/// Ring buffer for PTY output replay on reconnect.
const SCROLLBACK_BYTES: usize = 64 * 1024; // 64 KB

#[derive(Clone)]
pub struct ScrollbackBuf {
    buf: Arc<std::sync::Mutex<Vec<u8>>>,
}

impl ScrollbackBuf {
    fn new() -> Self {
        Self { buf: Arc::new(std::sync::Mutex::new(Vec::with_capacity(4096))) }
    }

    pub fn push(&self, data: &[u8]) {
        let mut buf = self.buf.lock().unwrap();
        buf.extend_from_slice(data);
        if buf.len() > SCROLLBACK_BYTES {
            let drain = buf.len() - SCROLLBACK_BYTES;
            buf.drain(..drain);
        }
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.lock().unwrap().clone()
    }
}

struct ManagedShell {
    info: ShellInfo,
    master: Box<dyn MasterPty + Send>,
    session_id: Option<String>,
    /// Stored after first take — shared via Arc so reconnects can reuse it.
    /// Uses std::sync::Mutex since PTY writes are blocking.
    writer: Option<Arc<std::sync::Mutex<Box<dyn Write + Send>>>>,
    scrollback: ScrollbackBuf,
}

/// Shell death event
#[derive(Debug, Clone)]
pub struct ShellDeath {
    pub shell_id: String,
    pub session_id: Option<String>,
}

#[derive(Clone)]
pub struct ShellManager {
    shells: Arc<Mutex<HashMap<String, ManagedShell>>>,
    death_tx: broadcast::Sender<ShellDeath>,
    default_shell: String,
}

impl ShellManager {
    pub fn new() -> Self {
        Self::with_default_shell("/bin/zsh".into())
    }

    pub fn with_default_shell(default_shell: String) -> Self {
        let (death_tx, _) = broadcast::channel(64);
        Self {
            shells: Arc::new(Mutex::new(HashMap::new())),
            death_tx,
            default_shell,
        }
    }

    pub fn on_death(&self) -> broadcast::Receiver<ShellDeath> {
        self.death_tx.subscribe()
    }

    /// Associate a hermytt session_id with a shell (called after bridge attach)
    pub async fn set_session_id(&self, shell_id: &str, session_id: &str) {
        if let Some(shell) = self.shells.lock().await.get_mut(shell_id) {
            shell.session_id = Some(session_id.to_string());
        }
    }

    /// Get the scrollback buffer for a shell
    pub async fn get_scrollback(&self, shell_id: &str) -> Option<ScrollbackBuf> {
        self.shells.lock().await.get(shell_id).map(|s| s.scrollback.clone())
    }

    /// Get session_id for a shell
    pub async fn get_session_id(&self, shell_id: &str) -> Option<String> {
        self.shells.lock().await.get(shell_id)
            .and_then(|s| s.session_id.clone())
    }

    /// Find shell_id by session_id
    pub async fn shell_id_by_session(&self, session_id: &str) -> Option<String> {
        self.shells.lock().await.values()
            .find(|s| s.session_id.as_deref() == Some(session_id))
            .map(|s| s.info.id.clone())
    }

    pub async fn spawn_with_limits(
        &self,
        req: SpawnRequest,
        max_shells: usize,
        allowed_hosts: &[String],
    ) -> Result<String, Error> {
        // Enforce shell count limit
        {
            let shells = self.shells.lock().await;
            if shells.len() >= max_shells {
                return Err(Error::SpawnFailed(format!(
                    "shell limit reached ({max_shells})"
                )));
            }
        }

        // Validate shell binary against allowlist
        if let Some(ref shell) = req.shell {
            if !ALLOWED_SHELLS.contains(&shell.as_str()) {
                return Err(Error::SpawnFailed(format!(
                    "shell not allowed: {shell} (allowed: bash, zsh, fish, sh, dash, csh, tcsh, ksh)"
                )));
            }
        }

        // Validate host against config allowlist
        if let Some(ref host) = req.host {
            if !allowed_hosts.is_empty() && !allowed_hosts.iter().any(|h| h == host) {
                return Err(Error::SpawnFailed(format!(
                    "host not in allowlist: {host}"
                )));
            }
        }

        // Validate name length
        if let Some(ref name) = req.name {
            if name.len() > MAX_NAME_LEN {
                return Err(Error::SpawnFailed(format!(
                    "name too long ({} > {MAX_NAME_LEN})", name.len()
                )));
            }
        }

        // Validate cwd doesn't traverse outside home or reasonable paths
        if let Some(ref cwd) = req.cwd {
            let expanded = expand_tilde(cwd);
            let path = std::path::Path::new(&expanded);
            // Block obvious sensitive paths
            if expanded.starts_with("/proc")
                || expanded.starts_with("/sys")
                || expanded == "/root"
                || expanded.starts_with("/root/")
            {
                return Err(Error::SpawnFailed(format!(
                    "cwd not allowed: {cwd}"
                )));
            }
            // Ensure path is absolute after expansion
            if !path.is_absolute() && !cwd.starts_with("~/") {
                return Err(Error::SpawnFailed(
                    "cwd must be an absolute path or start with ~/".into()
                ));
            }
        }

        self.spawn_inner(req).await
    }

    pub async fn spawn(&self, req: SpawnRequest) -> Result<String, Error> {
        self.spawn_inner(req).await
    }

    async fn spawn_inner(&self, req: SpawnRequest) -> Result<String, Error> {
        let id = next_id();
        let name = req.name.unwrap_or_else(|| id.clone());

        let shell_type = if req.agent.is_some() {
            ShellType::Agent
        } else if req.host.is_some() {
            ShellType::Remote
        } else if req.cmd.is_some() {
            ShellType::Command
        } else {
            ShellType::Local
        };

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| Error::SpawnFailed(e.to_string()))?;

        let mut cmd = match &shell_type {
            ShellType::Local => {
                let shell = req.shell.as_deref().unwrap_or(&self.default_shell);
                let mut cmd = CommandBuilder::new(shell);
                // Spawn as login shell so it sources user profile (.zprofile, .bash_profile)
                cmd.arg("-l");
                cmd
            }
            ShellType::Remote => {
                let mut cmd = CommandBuilder::new("ssh");
                cmd.arg("-t");
                cmd.arg(req.host.as_ref().unwrap());
                if let Some(c) = &req.cmd {
                    cmd.arg(c);
                }
                cmd
            }
            ShellType::Agent => {
                let mut cmd = CommandBuilder::new("claude");
                cmd.arg("--agent");
                cmd.arg(req.agent.as_ref().unwrap());
                cmd
            }
            ShellType::Command => {
                let mut cmd = CommandBuilder::new("sh");
                cmd.arg("-c");
                cmd.arg(req.cmd.as_ref().unwrap());
                cmd
            }
        };

        // Set TERM to xterm-256color — universally available, including macOS
        // which lacks tmux-256color terminfo. Prevents zsh ZLE dumb mode.
        cmd.env("TERM", "xterm-256color");

        // Ensure PATH includes Homebrew and common locations.
        // LaunchDaemons only get /usr/bin:/bin:/usr/sbin:/sbin.
        // Login shell will layer the user's profile on top.
        let path = std::env::var("PATH").unwrap_or_default();
        if !path.contains("/opt/homebrew") {
            cmd.env("PATH", format!(
                "/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:{}",
                path
            ));
        }

        if let Some(cwd) = &req.cwd {
            cmd.cwd(expand_tilde(cwd));
        }

        let child = pair.slave.spawn_command(cmd)
            .map_err(|e| Error::SpawnFailed(e.to_string()))?;
        drop(pair.slave);

        let managed = ManagedShell {
            info: ShellInfo { id: id.clone(), name, shell_type, status: ShellStatus::Running },
            master: pair.master,
            session_id: None,
            writer: None,
            scrollback: ScrollbackBuf::new(),
        };

        self.shells.lock().await.insert(id.clone(), managed);
        tracing::info!(shell_id = %id, "spawned");

        // Watch for child exit (poll-based so tokio can shut down cleanly)
        let shells = self.shells.clone();
        let death_tx = self.death_tx.clone();
        let watch_id = id.clone();
        tokio::spawn(async move {
            let mut child = child;
            loop {
                let exited = tokio::task::spawn_blocking({
                    let mut c = child;
                    move || {
                        let result = c.try_wait();
                        (c, result)
                    }
                }).await;
                match exited {
                    Ok((c, Ok(Some(_)))) => {
                        // Child exited
                        if let Some(shell) = shells.lock().await.remove(&watch_id) {
                            tracing::info!(shell_id = %watch_id, "shell exited");
                            let _ = death_tx.send(ShellDeath {
                                shell_id: watch_id,
                                session_id: shell.session_id,
                            });
                        }
                        return;
                    }
                    Ok((c, Ok(None))) => {
                        // Still running
                        child = c;
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    }
                    Ok((_, Err(_))) | Err(_) => return,
                }
            }
        });

        Ok(id)
    }

    pub async fn list(&self) -> Vec<ShellInfo> {
        self.shells.lock().await.values().map(|s| s.info.clone()).collect()
    }

    pub async fn kill(&self, id: &str) -> Result<ShellInfo, Error> {
        let shell = self.shells.lock().await.remove(id)
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        drop(shell.master);
        let mut info = shell.info;
        info.status = ShellStatus::Dead;
        Ok(info)
    }

    pub async fn resize(&self, id: &str, rows: u16, cols: u16) -> Result<(), Error> {
        let shells = self.shells.lock().await;
        let shell = shells.get(id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        shell.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| Error::SpawnFailed(e.to_string()))
    }

    /// Extract a cloned reader handle. Lock is released immediately.
    pub async fn get_reader(&self, id: &str) -> Result<Box<dyn Read + Send>, Error> {
        let shells = self.shells.lock().await;
        let shell = shells.get(id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        shell.master.try_clone_reader()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
    }

    /// Extract a writer handle. Lock is released immediately.
    pub async fn get_writer(&self, id: &str) -> Result<Box<dyn Write + Send>, Error> {
        let shells = self.shells.lock().await;
        let shell = shells.get(id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        shell.master.take_writer()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
    }

    /// Get both reader and writer in a single lock (avoids race between the two calls).
    pub async fn get_reader_writer(&self, id: &str) -> Result<(Box<dyn Read + Send>, Arc<std::sync::Mutex<Box<dyn Write + Send>>>), Error> {
        let mut shells = self.shells.lock().await;
        let shell = shells.get_mut(id).ok_or_else(|| Error::NotFound(id.to_string()))?;
        let reader = shell.master.try_clone_reader()
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        // First call takes the writer from the master; subsequent calls reuse the stored Arc.
        let writer = match &shell.writer {
            Some(w) => w.clone(),
            None => {
                let w = shell.master.take_writer()
                    .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
                let shared = Arc::new(std::sync::Mutex::new(w));
                shell.writer = Some(shared.clone());
                shared
            }
        };
        Ok((reader, writer))
    }
}

pub(crate) fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}/{rest}", home.to_string_lossy());
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_with_subpath() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/foo"), format!("{home}/foo"));
    }

    #[test]
    fn expand_tilde_absolute_unchanged() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn expand_tilde_relative_unchanged() {
        assert_eq!(expand_tilde("relative"), "relative");
    }

    #[test]
    fn expand_tilde_empty() {
        assert_eq!(expand_tilde(""), "");
    }

    #[test]
    fn expand_tilde_just_tilde() {
        // "~" without trailing slash is not expanded
        assert_eq!(expand_tilde("~"), "~");
    }

    #[test]
    fn next_id_unique() {
        let a = next_id();
        let b = next_id();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn manager_new_is_empty() {
        let mgr = ShellManager::new();
        assert!(mgr.list().await.is_empty());
    }

    #[tokio::test]
    async fn spawn_echo_returns_ok() {
        let mgr = ShellManager::new();
        let id = mgr.spawn(SpawnRequest {
            name: Some("test-echo".into()),
            shell: None,
            cwd: None,
            host: None,
            agent: None,
            cmd: Some("echo hello".into()),
        }).await;
        assert!(id.is_ok());
        assert!(!id.unwrap().is_empty());
    }

    #[tokio::test]
    async fn spawn_then_list_shows_shell() {
        let mgr = ShellManager::new();
        let id = mgr.spawn(SpawnRequest {
            name: Some("lister".into()),
            shell: None,
            cwd: None,
            host: None,
            agent: None,
            cmd: Some("echo hello".into()),
        }).await.unwrap();
        let shells = mgr.list().await;
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].id, id);
        assert_eq!(shells[0].name, "lister");
    }

    #[tokio::test]
    async fn spawn_then_kill_returns_dead() {
        let mgr = ShellManager::new();
        let id = mgr.spawn(SpawnRequest {
            name: None,
            shell: None,
            cwd: None,
            host: None,
            agent: None,
            cmd: Some("echo hello".into()),
        }).await.unwrap();
        let info = mgr.kill(&id).await.unwrap();
        assert!(matches!(info.status, ShellStatus::Dead));
        assert!(mgr.list().await.is_empty());
    }

    #[tokio::test]
    async fn kill_nonexistent_returns_not_found() {
        let mgr = ShellManager::new();
        let err = mgr.kill("nope").await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[tokio::test]
    async fn resize_nonexistent_returns_not_found() {
        let mgr = ShellManager::new();
        let err = mgr.resize("nope", 24, 80).await.unwrap_err();
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[tokio::test]
    async fn spawn_two_kill_one_list_shows_survivor() {
        let mgr = ShellManager::new();
        let id1 = mgr.spawn(SpawnRequest {
            name: Some("a".into()),
            shell: None, cwd: None, host: None, agent: None,
            cmd: Some("sleep 60".into()),
        }).await.unwrap();
        let id2 = mgr.spawn(SpawnRequest {
            name: Some("b".into()),
            shell: None, cwd: None, host: None, agent: None,
            cmd: Some("sleep 60".into()),
        }).await.unwrap();
        mgr.kill(&id1).await.unwrap();
        let shells = mgr.list().await;
        assert_eq!(shells.len(), 1);
        assert_eq!(shells[0].id, id2);
    }

    #[test]
    fn type_detection_agent() {
        let req = SpawnRequest {
            name: None, shell: None, cwd: None, host: None,
            agent: Some("infra".into()), cmd: None,
        };
        assert!(req.agent.is_some());
    }

    #[test]
    fn type_detection_remote() {
        let req = SpawnRequest {
            name: None, shell: None, cwd: None,
            host: Some("user@host".into()), agent: None, cmd: None,
        };
        assert!(req.host.is_some());
    }

    #[test]
    fn type_detection_command() {
        let req = SpawnRequest {
            name: None, shell: None, cwd: None, host: None, agent: None,
            cmd: Some("ls".into()),
        };
        assert!(req.cmd.is_some());
    }

    #[test]
    fn type_detection_local() {
        let req = SpawnRequest {
            name: None, shell: None, cwd: None,
            host: None, agent: None, cmd: None,
        };
        assert!(req.host.is_none() && req.agent.is_none() && req.cmd.is_none());
    }

    #[tokio::test]
    async fn get_reader_writer_returns_both() {
        let mgr = ShellManager::new();
        let id = mgr.spawn(SpawnRequest {
            name: Some("rw-test".into()),
            shell: None, cwd: None, host: None, agent: None,
            cmd: Some("sleep 60".into()),
        }).await.unwrap();
        let result = mgr.get_reader_writer(&id).await;
        assert!(result.is_ok(), "get_reader_writer should succeed");
        // Clean up
        let _ = mgr.kill(&id).await;
    }

    #[tokio::test]
    async fn shell_id_by_session_finds_shell() {
        let mgr = ShellManager::new();
        let id = mgr.spawn(SpawnRequest {
            name: Some("session-lookup".into()),
            shell: None, cwd: None, host: None, agent: None,
            cmd: Some("sleep 60".into()),
        }).await.unwrap();
        mgr.set_session_id(&id, "my-session-123").await;
        let found = mgr.shell_id_by_session("my-session-123").await;
        assert_eq!(found, Some(id.clone()));
        let _ = mgr.kill(&id).await;
    }

    #[tokio::test]
    async fn shell_id_by_session_returns_none_for_unknown() {
        let mgr = ShellManager::new();
        let found = mgr.shell_id_by_session("nonexistent-session").await;
        assert_eq!(found, None);
    }

    #[tokio::test]
    async fn on_death_receives_notification() {
        let mgr = ShellManager::new();
        let mut death_rx = mgr.on_death();
        // Use sleep so we have time to set session_id before it exits
        let id = mgr.spawn(SpawnRequest {
            name: Some("death-test".into()),
            shell: None, cwd: None, host: None, agent: None,
            cmd: Some("sleep 0.2".into()),
        }).await.unwrap();
        mgr.set_session_id(&id, "death-session").await;
        // Wait for the short-lived command to exit and the death watcher to fire
        let death = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            death_rx.recv(),
        ).await.expect("timed out waiting for death notification")
         .expect("death channel closed");
        assert_eq!(death.shell_id, id);
        assert_eq!(death.session_id, Some("death-session".into()));
    }
}
