use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::logger;

/// pending 文件路径
fn pending_path() -> PathBuf {
    let home = dirs::home_dir().expect("Cannot determine home directory");
    home.join(".claude-hook-monitor")
        .join("pending-approval.json")
}

/// response 文件路径
fn response_path() -> PathBuf {
    let home = dirs::home_dir().expect("Cannot determine home directory");
    home.join(".claude-hook-monitor")
        .join("approval-response.json")
}

/// 清理 IPC 文件
fn cleanup() {
    let _ = fs::remove_file(pending_path());
    let _ = fs::remove_file(response_path());
}

/// 处理 PermissionRequest：写 pending 文件，等待 Cat 的响应
pub fn handle_approve() {
    // 读取 stdin
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }

    let raw: Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => return,
    };

    // 同时写入 events.jsonl（供 Cat 状态轮询用）
    if let Ok(event) = serde_json::from_str::<crate::events::HookEvent>(&input) {
        let _ = logger::write_event(&event, &raw);
    }

    // 提取关键信息
    let tool_name = raw
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let tool_input = raw.get("tool_input").cloned().unwrap_or(json!({}));

    // 生成摘要
    let summary = if let Some(cmd) = tool_input.get("command").and_then(|v| v.as_str()) {
        format!("{}: {}", tool_name, cmd)
    } else if let Some(path) = tool_input.get("file_path").and_then(|v| v.as_str()) {
        format!("{}: {}", tool_name, path)
    } else {
        tool_name.to_string()
    };

    // 清理旧文件
    cleanup();

    // 确保目录存在
    let _ = logger::ensure_log_dir();

    // 写 pending 文件
    let pending = json!({
        "tool_name": tool_name,
        "summary": summary,
        "tool_input": tool_input,
        "timestamp": chrono::Local::now().to_rfc3339(),
    });

    if fs::write(
        pending_path(),
        serde_json::to_string_pretty(&pending).unwrap(),
    )
    .is_err()
    {
        // 写失败，默认放行
        return;
    }

    // 轮询等待 response 文件（100ms 间隔，超时 10 分钟）
    let start = Instant::now();
    let timeout = Duration::from_secs(600);
    let poll_interval = Duration::from_millis(100);

    loop {
        if start.elapsed() > timeout {
            // 超时，清理并默认不输出（Claude Code 正常显示权限对话框）
            cleanup();
            return;
        }

        if response_path().exists() {
            break;
        }

        thread::sleep(poll_interval);
    }

    // 读取 response
    let response_str = match fs::read_to_string(response_path()) {
        Ok(s) => s,
        Err(_) => {
            cleanup();
            return;
        }
    };

    let response: Value = match serde_json::from_str(&response_str) {
        Ok(v) => v,
        Err(_) => {
            cleanup();
            return;
        }
    };

    let allow = response
        .get("allow")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 输出决策到 stdout
    let decision = if allow {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {
                    "behavior": "allow"
                }
            }
        })
    } else {
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": {
                    "behavior": "deny",
                    "message": "Denied by desktop cat"
                }
            }
        })
    };

    // 输出到 stdout（这是 Claude Code 读取的决策）
    println!("{}", serde_json::to_string(&decision).unwrap());

    // 清理
    cleanup();
}
