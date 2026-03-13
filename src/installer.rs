use std::env;
use std::fs;
use std::path::PathBuf;

use chrono::Local;
use serde_json::{json, Value};

/// 安装范围
pub enum InstallScope {
    /// 用户级别：~/.claude/settings.json
    User,
    /// 项目级别：.claude/settings.json
    Project,
}

/// 获取当前二进制的绝对路径
fn get_binary_path() -> String {
    env::current_exe()
        .unwrap_or_else(|_| PathBuf::from("claude-hook-monitor"))
        .to_string_lossy()
        .to_string()
}

/// 获取目标 settings.json 路径
fn settings_path(scope: &InstallScope) -> PathBuf {
    match scope {
        InstallScope::User => {
            let home = dirs::home_dir().expect("Cannot determine home directory");
            home.join(".claude").join("settings.json")
        }
        InstallScope::Project => PathBuf::from(".claude").join("settings.json"),
    }
}

/// 生成所有事件的 hook 配置
fn generate_hooks_config(binary_path: &str) -> Value {
    let listen_cmd = format!("{} listen", binary_path);
    let approve_cmd = format!("{} approve", binary_path);

    // 有 matcher 的事件（listen）
    let with_matcher = |matcher: &str| -> Value {
        json!([{
            "matcher": matcher,
            "hooks": [{
                "type": "command",
                "command": &listen_cmd
            }]
        }])
    };

    // 无 matcher 的事件（listen）
    let without_matcher = || -> Value {
        json!([{
            "hooks": [{
                "type": "command",
                "command": &listen_cmd
            }]
        }])
    };

    json!({
        "SessionStart": with_matcher(""),
        "InstructionsLoaded": without_matcher(),
        "UserPromptSubmit": without_matcher(),
        "PreToolUse": with_matcher(""),
        "PermissionRequest": json!([{
            "matcher": "",
            "hooks": [
                {
                    "type": "command",
                    "command": &listen_cmd
                },
                {
                    "type": "command",
                    "command": &approve_cmd
                }
            ]
        }]),
        "PostToolUse": with_matcher(""),
        "PostToolUseFailure": with_matcher(""),
        "Notification": with_matcher(""),
        "SubagentStart": with_matcher(""),
        "SubagentStop": with_matcher(""),
        "Stop": without_matcher(),
        "TeammateIdle": without_matcher(),
        "TaskCompleted": without_matcher(),
        "ConfigChange": with_matcher(""),
        "WorktreeCreate": without_matcher(),
        "WorktreeRemove": without_matcher(),
        "PreCompact": with_matcher(""),
        "SessionEnd": with_matcher(""),
    })
}

/// 安装 hook 配置到 settings.json
pub fn install(scope: InstallScope) -> Result<String, String> {
    let binary_path = get_binary_path();
    let target_path = settings_path(&scope);
    let hooks_config = generate_hooks_config(&binary_path);

    // 确保目录存在
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Cannot create directory: {}", e))?;
    }

    // 备份已有配置
    let backup_path = if target_path.exists() {
        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let backup = target_path.with_extension(format!("backup.{}.json", timestamp));
        fs::copy(&target_path, &backup)
            .map_err(|e| format!("Cannot backup file: {}", e))?;
        Some(backup)
    } else {
        None
    };

    // 读取已有配置
    let mut settings: Value = if target_path.exists() {
        let content =
            fs::read_to_string(&target_path).map_err(|e| format!("Cannot read file: {}", e))?;
        serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    // 合并 hooks 配置
    let settings_obj = settings
        .as_object_mut()
        .ok_or("Settings is not a JSON object")?;

    if settings_obj.contains_key("hooks") {
        let existing_hooks = settings_obj
            .get_mut("hooks")
            .unwrap()
            .as_object_mut()
            .ok_or("hooks is not a JSON object")?;

        // 仅添加缺失的事件
        if let Some(new_hooks) = hooks_config.as_object() {
            for (event_name, config) in new_hooks {
                if !existing_hooks.contains_key(event_name) {
                    existing_hooks.insert(event_name.clone(), config.clone());
                }
            }
        }
    } else {
        settings_obj.insert("hooks".to_string(), json!({ "hooks": hooks_config }));
        // 修正：直接设置 hooks
        settings_obj.insert("hooks".to_string(), hooks_config);
    }

    // 写入文件
    let output = serde_json::to_string_pretty(&settings)
        .map_err(|e| format!("Cannot serialize JSON: {}", e))?;
    fs::write(&target_path, output).map_err(|e| format!("Cannot write file: {}", e))?;

    let mut msg = format!(
        "Installed hooks to: {}\nBinary: {}",
        target_path.display(),
        binary_path
    );
    if let Some(backup) = backup_path {
        msg.push_str(&format!("\nBackup saved to: {}", backup.display()));
    }

    Ok(msg)
}
