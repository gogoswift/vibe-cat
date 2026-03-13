use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::thread;
use std::time::Duration;

use colored::*;

use crate::logger::{log_file_path, LogEntry};

/// 为事件类型上色
fn colorize_event_type(event_type: &str) -> ColoredString {
    match event_type {
        "SessionStart" => event_type.green().bold(),
        "SessionEnd" => event_type.red().bold(),
        "PreToolUse" => event_type.cyan(),
        "PostToolUse" => event_type.blue(),
        "PostToolUseFailure" => event_type.red(),
        "PermissionRequest" => event_type.yellow().bold(),
        "Notification" => event_type.yellow(),
        "SubagentStart" => event_type.magenta(),
        "SubagentStop" => event_type.magenta(),
        "Stop" => event_type.red(),
        "UserPromptSubmit" => event_type.green(),
        "InstructionsLoaded" => event_type.white().dimmed(),
        "TeammateIdle" => event_type.yellow(),
        "TaskCompleted" => event_type.green().bold(),
        "ConfigChange" => event_type.yellow(),
        "WorktreeCreate" => event_type.cyan(),
        "WorktreeRemove" => event_type.cyan(),
        "PreCompact" => event_type.white().dimmed(),
        "conversation_starts" => event_type.green().bold(),
        "api_request" => event_type.cyan(),
        "tool_decision" => event_type.cyan(),
        "tool_result" => event_type.blue(),
        "sse_event" => event_type.white().dimmed(),
        _ => event_type.normal(),
    }
}

/// 格式化单条日志
fn format_entry(entry: &LogEntry) -> String {
    let time = if entry.timestamp.len() >= 19 {
        &entry.timestamp[11..19] // HH:MM:SS
    } else {
        &entry.timestamp
    };

    let source_tag = match entry.source.as_str() {
        "cx" => "[CX]".magenta().to_string(),
        _ => "[CC]".cyan().to_string(),
    };

    let event_colored = colorize_event_type(&entry.event_type);

    let tool_part = entry
        .tool_name
        .as_ref()
        .map(|t| format!(" [{}]", t.yellow()))
        .unwrap_or_default();

    format!(
        "{} {} {:>22}{} {}",
        time.dimmed(),
        source_tag,
        event_colored,
        tool_part,
        entry.summary.white()
    )
}

/// 实时跟踪日志文件（类似 tail -f）
pub fn tail_log(filter: Option<&str>) {
    let log_path = log_file_path();

    if !log_path.exists() {
        eprintln!(
            "{}",
            "No log file found. Run 'claude-hook-monitor install' first.".red()
        );
        eprintln!("Expected: {}", log_path.display());
        return;
    }

    println!(
        "{}",
        format!("Tailing: {}", log_path.display()).dimmed()
    );
    if let Some(f) = filter {
        println!("{}", format!("Filter: {}", f).dimmed());
    }
    println!("{}", "─".repeat(80).dimmed());

    // 先显示最近 10 条
    let file = File::open(&log_path).expect("Cannot open log file");
    let reader = BufReader::new(&file);
    let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    let start = if lines.len() > 10 { lines.len() - 10 } else { 0 };

    for line in &lines[start..] {
        if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
            if should_show(&entry, filter) {
                println!("{}", format_entry(&entry));
            }
        }
    }

    println!("{}", "─ waiting for new events ─".dimmed());

    // 持续跟踪新行
    let mut file = File::open(&log_path).expect("Cannot open log file");
    file.seek(SeekFrom::End(0)).expect("Cannot seek to end");

    let mut reader = BufReader::new(file);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // 没有新内容，等待
                thread::sleep(Duration::from_millis(200));
            }
            Ok(_) => {
                let line = line.trim();
                if !line.is_empty() {
                    if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                        if should_show(&entry, filter) {
                            println!("{}", format_entry(&entry));
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Read error: {}", e);
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// 根据过滤器判断是否显示
fn should_show(entry: &LogEntry, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => entry.event_type.to_lowercase().contains(&f.to_lowercase()),
    }
}

/// 显示状态摘要
pub fn show_status() {
    let log_path = log_file_path();

    if !log_path.exists() {
        eprintln!(
            "{}",
            "No log file found. Run 'claude-hook-monitor install' first.".red()
        );
        return;
    }

    let entries = crate::logger::read_recent_entries(100).unwrap_or_default();

    if entries.is_empty() {
        println!("{}", "No events recorded yet.".yellow());
        return;
    }

    // 最近事件时间
    if let Some(last) = entries.last() {
        println!("{}: {}", "Last event".bold(), last.timestamp);
        println!("{}: {}", "Session".bold(), last.session_id);
    }

    // 统计各事件类型
    println!("\n{}", "Event counts (last 100):".bold());
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for entry in &entries {
        *counts.entry(&entry.event_type).or_insert(0) += 1;
    }

    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));

    for (event_type, count) in sorted {
        println!(
            "  {:>22}: {}",
            colorize_event_type(event_type),
            count.to_string().bold()
        );
    }

    // 最近 5 条
    println!("\n{}", "Recent events:".bold());
    let recent_start = if entries.len() > 5 {
        entries.len() - 5
    } else {
        0
    };
    for entry in &entries[recent_start..] {
        println!("  {}", format_entry(entry));
    }
}
