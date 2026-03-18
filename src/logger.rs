use std::fs::{self, OpenOptions};
use std::io::{Read as _, Seek, SeekFrom, Write};
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

/// 日志文件超过 50MB 时自动轮转：只保留尾部 5MB，旧内容丢弃
fn maybe_rotate_log() -> std::io::Result<()> {
    let log_path = log_file_path();
    if !log_path.exists() {
        return Ok(());
    }
    let metadata = fs::metadata(&log_path)?;
    // 50MB 阈值
    if metadata.len() < 50 * 1024 * 1024 {
        return Ok(());
    }
    // 只读取尾部 5MB 保留
    let keep_size: u64 = 5 * 1024 * 1024;
    let mut file = fs::File::open(&log_path)?;
    let offset = metadata.len().saturating_sub(keep_size);
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = String::with_capacity(keep_size as usize);
    file.read_to_string(&mut buf)?;
    drop(file);
    // 跳过第一行（可能不完整）
    if let Some(pos) = buf.find('\n') {
        buf = buf[pos + 1..].to_string();
    }
    fs::write(&log_path, buf)?;
    Ok(())
}

/// 将事件写入日志文件
pub fn write_event(event: &HookEvent, raw_json: &Value) -> std::io::Result<()> {
    ensure_log_dir()?;
    let _ = maybe_rotate_log();

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

/// 读取最近 N 条日志（从文件尾部读取，避免加载整个文件到内存）
pub fn read_recent_entries(count: usize) -> std::io::Result<Vec<LogEntry>> {
    let log_path = log_file_path();
    if !log_path.exists() {
        return Ok(Vec::new());
    }

    let mut file = fs::File::open(&log_path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 {
        return Ok(Vec::new());
    }

    // 从文件尾部读取足够大的块（每行约 500 字节，多读一些确保够用）
    let read_size = ((count as u64) * 600).min(file_len);
    let offset = file_len.saturating_sub(read_size);
    file.seek(SeekFrom::Start(offset))?;

    let mut buf = String::with_capacity(read_size as usize);
    file.read_to_string(&mut buf)?;

    let lines: Vec<&str> = buf.lines().collect();
    let start = if lines.len() > count {
        lines.len() - count
    } else {
        // 如果从中间开始读，跳过第一行（可能不完整）
        if offset > 0 { 1 } else { 0 }
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
    let _ = maybe_rotate_log();
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
