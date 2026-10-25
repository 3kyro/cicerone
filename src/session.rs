use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A running Claude Code session, from `$CLAUDE_CODE_MESSAGING_SOCKET` or the
/// `~/.claude/sessions/` registry.
#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    cwd: PathBuf,
    socket: PathBuf,
    token: Option<String>,
    updated_at: u64,
}

fn sessions_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("HOME")?).join(".claude").join("sessions"))
}

fn parse_session(v: &Value) -> Option<Session> {
    Some(Session {
        name: v["name"].as_str().unwrap_or("claude").to_string(),
        cwd: PathBuf::from(v["cwd"].as_str().unwrap_or("")),
        socket: PathBuf::from(v["messagingSocketPath"].as_str()?),
        token: None,
        updated_at: v["updatedAt"].as_u64().unwrap_or(0),
    })
}

/// The peer auth token Claude Code publishes for each session, in a
/// `<pid>.<hash>.key` file next to the session's registry entry.
fn key_file_token(dir: &Path, pid: u64) -> Option<String> {
    let prefix = format!("{pid}.");
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(&prefix)
            && name.ends_with(".key")
            && let Ok(token) = std::fs::read_to_string(entry.path())
        {
            let token = token.trim().to_string();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }
    None
}

fn registry_sessions(dir: &Path) -> Vec<Session> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Some(v) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        if let Some(mut s) = parse_session(&v) {
            s.token = v["pid"].as_u64().and_then(|pid| key_file_token(dir, pid));
            sessions.push(s);
        }
    }
    sessions
}

/// The session that opened cicerone, else the most recent live session
/// running inside `root`.
pub fn discover(root: &Path) -> Option<Session> {
    let registry = sessions_dir().as_deref().map(registry_sessions).unwrap_or_default();
    if let Some(socket) = std::env::var_os("CLAUDE_CODE_MESSAGING_SOCKET") {
        let socket = PathBuf::from(socket);
        let entry = registry.into_iter().find(|s| s.socket == socket);
        // the registry key is the peer token, valid for non-child senders;
        // the env token only works where child process evidence does
        let token = entry.as_ref().and_then(|s| s.token.clone()).or_else(|| {
            std::env::var("CLAUDE_CODE_MESSAGING_TOKEN")
                .ok()
                .filter(|t| !t.is_empty())
        });
        let mut s = entry.unwrap_or_else(|| Session {
            name: "claude".into(),
            cwd: root.to_path_buf(),
            socket,
            token: None,
            updated_at: 0,
        });
        s.token = token;
        return Some(s);
    }
    registry
        .into_iter()
        .filter(|s| s.cwd.starts_with(root) && UnixStream::connect(&s.socket).is_ok())
        .max_by_key(|s| s.updated_at)
}

/// Newline-delimited JSON over the session's inbox socket: an auth frame
/// (when we have a token), then the message.
pub fn send(s: &Session, text: &str) -> Result<()> {
    let mut stream =
        UnixStream::connect(&s.socket).with_context(|| format!("cannot reach Claude session '{}'", s.name))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    if let Some(token) = &s.token {
        writeln!(stream, "{}", json!({"type": "auth", "token": token}))?;
    }
    writeln!(
        stream,
        "{}",
        json!({"type": "user", "message": {"role": "user", "content": text}})
    )
    .with_context(|| format!("failed to send to Claude session '{}'", s.name))?;
    stream.shutdown(Shutdown::Write)?;
    // drain until the server closes, so it reads the frames before we drop
    let mut buf = [0u8; 256];
    while matches!(stream.read(&mut buf), Ok(n) if n > 0) {}
    Ok(())
}
