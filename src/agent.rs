use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;

#[derive(Debug)]
pub enum Event {
    Text(String),
    Tool(String),
    TurnDone { ok: bool },
    Exited(String),
}

pub struct Agent {
    child: Child,
    stdin: ChildStdin,
}

/// The diff under review can carry instructions, so the agent only gets
/// read-only file tools: no shell, no MCP servers, and no settings files
/// (so no hooks) from the user or from the repository under review.
const TOOLS: &str = "Read,Grep,Glob";

pub struct Options<'a> {
    pub cwd: &'a Path,
    pub session: &'a str,
    pub resume: bool,
    pub system: &'a str,
    pub model: Option<&'a str>,
    pub log: Option<&'a Path>,
    pub files: Option<&'a Path>,
}

impl Agent {
    pub fn start(o: Options, tx: Sender<Event>) -> Result<Agent> {
        let mut cmd = Command::new("claude");
        cmd.current_dir(o.cwd)
            .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json"])
            .args(["--include-partial-messages", "--verbose"])
            .args([if o.resume { "--resume" } else { "--session-id" }, o.session])
            .args(["--append-system-prompt", o.system])
            .args(["--restricted", "--strict-mcp-config"])
            .args(["--tools", TOOLS])
            .args(["--allowedTools", &TOOLS.replace(',', " ")]);
        if let Some(d) = o.files {
            cmd.arg("--add-dir").arg(d);
        }
        if let Some(m) = o.model {
            cmd.args(["--model", m]);
        }
        let stderr = match o.log.and_then(|p| std::fs::File::create(p).ok()) {
            Some(f) => Stdio::from(f),
            None => Stdio::null(),
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .context("starting claude")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || read_events(stdout, tx));
        Ok(Agent { child, stdin })
    }

    pub fn send(&mut self, text: &str) -> Result<()> {
        let msg = json!({"type": "user", "message": {"role": "user", "content": text}});
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_events(stdout: std::process::ChildStdout, tx: Sender<Event>) {
    let mut last_error = String::new();
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let event = match v["type"].as_str() {
            Some("stream_event") => {
                let e = &v["event"];
                match (e["type"].as_str(), e["delta"]["type"].as_str()) {
                    (Some("content_block_delta"), Some("text_delta")) => {
                        e["delta"]["text"].as_str().map(|t| Event::Text(t.to_string()))
                    }
                    _ => None,
                }
            }
            Some("assistant") => v["message"]["content"]
                .as_array()
                .and_then(|cs| cs.iter().find(|c| c["type"] == "tool_use"))
                .map(|c| Event::Tool(tool_summary(c))),
            Some("result") => {
                let ok = !v["is_error"].as_bool().unwrap_or(false);
                if !ok {
                    last_error = v["result"].as_str().unwrap_or("error").to_string();
                }
                Some(Event::TurnDone { ok })
            }
            _ => None,
        };
        if let Some(e) = event
            && tx.send(e).is_err()
        {
            return;
        }
    }
    let _ = tx.send(Event::Exited(last_error));
}

fn tool_summary(c: &Value) -> String {
    let name = c["name"].as_str().unwrap_or("tool");
    let input = &c["input"];
    let arg = ["file_path", "pattern", "command", "path"]
        .iter()
        .find_map(|k| input[k].as_str())
        .unwrap_or("");
    let arg = arg.rsplit('/').next().unwrap_or(arg);
    let arg: String = arg.chars().take(60).collect();
    format!("{name} {arg}").trim().to_string()
}

pub fn new_session_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| {
            let n = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let h = format!("{n:032x}");
            format!("{}-{}-4{}-8{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
        })
}
