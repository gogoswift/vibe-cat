use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use chrono::Local;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::HookEvent;

/// 日志条目
#[derive(Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub timestamp: String,
    #[serde(default = "default_source")]
    pub source: String,
    pub event_type: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    pub summary: String,
    pub raw: Value,
}

fn default_source() -> String {
    "cc".to_string()
}

/// 获取日志文件路径
pub fn log_file_path() -> PathBuf {
    let home = dirs::home_dir().expect("Cannot determine home directory");
    home.join(".vibe-cat").join("events.jsonl")
}

/// 确保日志目录存在
pub fn ensure_log_dir() -> std::io::Result<()> {
    let path = log_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// 将事件写入日志文件
pub fn write_event(event: &HookEvent, raw_json: &Value) -> std::io::Result<()> {
    ensure_log_dir()?;

    let entry = LogEntry {
        timestamp: Local::now().to_rfc3339(),
        source: "cc".to_string(),
        event_type: event.event_type().to_string(),
        session_id: event.session_id().to_string(),
        tool_name: event.tool_name().map(|s| s.to_string()),
        summary: event.summary(),
        raw: raw_json.clone(),
    };

    let mut line = serde_json::to_string(&entry)?;
    line.push('\n');

    let log_path = log_file_path();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    // 文件锁防止并发写入交错
    file.lock_exclusive()?;
    file.write_all(line.as_bytes())?;
    file.unlock()?;

    Ok(())
}

/// 读取最近 N 条日志
pub fn read_recent_entries(count: usize) -> std::io::Result<Vec<LogEntry>> {
    let log_path = log_file_path();
    if !log_path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(&log_path)?;
    let lines: Vec<&str> = content.lines().collect();
    let start = if lines.len() > count {
        lines.len() - count
    } else {
        0
    };

    let mut entries = Vec::new();
    for line in &lines[start..] {
        if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
            entries.push(entry);
        }
    }

    Ok(entries)
}

/// 将 Codex 事件写入日志文件
pub fn write_codex_event(
    event_type: &str,
    session_id: &str,
    tool_name: Option<&str>,
    summary: &str,
    raw: Value,
) -> std::io::Result<()> {
    ensure_log_dir()?;
    let entry = LogEntry {
        timestamp: Local::now().to_rfc3339(),
        source: "cx".to_string(),
        event_type: event_type.to_string(),
        session_id: session_id.to_string(),
        tool_name: tool_name.map(|s| s.to_string()),
        summary: summary.to_string(),
        raw,
    };
    let mut line = serde_json::to_string(&entry)?;
    line.push('\n');
    let log_path = log_file_path();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    file.lock_exclusive()?;
    file.write_all(line.as_bytes())?;
    file.unlock()?;
    Ok(())
}
