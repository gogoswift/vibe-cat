use std::env;
use std::fs;
use std::path::PathBuf;

use chrono::Local;
use colored::*;
use serde_json::{json, Value};

/// 安装范围
pub enum InstallScope {
    /// 用户级别：~/.claude/settings.json
    User,
    /// 项目级别：.claude/settings.json
    Project,
}

/// 获取当前二进制的绝对路径
pub fn get_binary_path() -> String {
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

// ============================================================
// 自动检查与安装
// ============================================================

/// 检查 Claude Code hooks 是否已注册（包含当前二进制路径）
fn is_claude_hooks_installed() -> bool {
    let binary_path = get_binary_path();
    let target_path = settings_path(&InstallScope::User);

    if !target_path.exists() {
        return false;
    }

    let content = match fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let settings: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };

    // 检查 hooks 对象中是否有包含当前 binary_path 的 command
    match settings.get("hooks").and_then(|h| h.as_object()) {
        Some(hooks) => {
            let listen_cmd = format!("{} listen", binary_path);
            hooks
                .values()
                .any(|event_config| event_config.to_string().contains(&listen_cmd))
        }
        None => false,
    }
}

/// 自动检查并安装 Claude Code hooks（User 级别）
/// 返回 Ok(true) 表示执行了安装，Ok(false) 表示已安装无需操作
fn ensure_claude_hooks() -> Result<bool, String> {
    if is_claude_hooks_installed() {
        return Ok(false);
    }

    install(InstallScope::User)?;
    Ok(true)
}

// ============================================================
// Codex 配置
// ============================================================

/// 获取 Codex config.toml 路径
fn codex_config_path() -> PathBuf {
    let home = dirs::home_dir().expect("Cannot determine home directory");
    home.join(".codex").join("config.toml")
}

/// 检查 Codex OTel + notify 是否已配置
fn is_codex_configured() -> bool {
    let config_path = codex_config_path();

    if !config_path.exists() {
        return false;
    }

    let content = match fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let config: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(_) => return false,
    };

    let has_otel = config.get("otel").is_some();
    let has_notify = config.get("notify").is_some();

    has_otel && has_notify
}

/// 自动检查并配置 Codex OTel + notify
/// 返回 Ok(true) 表示执行了配置，Ok(false) 表示已配置无需操作
fn ensure_codex_config() -> Result<bool, String> {
    if is_codex_configured() {
        return Ok(false);
    }

    let binary_path = get_binary_path();
    let config_path = codex_config_path();

    // 确保目录存在
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Cannot create .codex directory: {}", e))?;
    }

    // 备份已有配置
    if config_path.exists() {
        let timestamp = Local::now().format("%Y%m%d_%H%M%S");
        let backup = config_path.with_extension(format!("backup.{}.toml", timestamp));
        fs::copy(&config_path, &backup)
            .map_err(|e| format!("Cannot backup config.toml: {}", e))?;
    }

    // 读取已有配置或创建空配置
    let mut config: toml::Value = if config_path.exists() {
        let content = fs::read_to_string(&config_path)
            .map_err(|e| format!("Cannot read config.toml: {}", e))?;
        content
            .parse()
            .map_err(|e| format!("Cannot parse config.toml: {}", e))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    let table = config
        .as_table_mut()
        .ok_or("config.toml root is not a table")?;

    // 添加 [otel] 段（如果不存在）
    if !table.contains_key("otel") {
        let mut otlp_http = toml::map::Map::new();
        otlp_http.insert(
            "endpoint".to_string(),
            toml::Value::String("http://localhost:4318/v1/logs".to_string()),
        );
        otlp_http.insert(
            "protocol".to_string(),
            toml::Value::String("binary".to_string()),
        );

        let mut exporter = toml::map::Map::new();
        exporter.insert("otlp-http".to_string(), toml::Value::Table(otlp_http));

        let mut otel = toml::map::Map::new();
        otel.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
        otel.insert("exporter".to_string(), toml::Value::Table(exporter));

        table.insert("otel".to_string(), toml::Value::Table(otel));
    }

    // 添加 notify（如果不存在）
    if !table.contains_key("notify") {
        table.insert(
            "notify".to_string(),
            toml::Value::Array(vec![
                toml::Value::String(binary_path),
                toml::Value::String("codex".to_string()),
            ]),
        );
    }

    // 写入文件
    let output =
        toml::to_string_pretty(&config).map_err(|e| format!("Cannot serialize TOML: {}", e))?;
    fs::write(&config_path, output)
        .map_err(|e| format!("Cannot write config.toml: {}", e))?;

    Ok(true)
}

// ============================================================
// 统一入口
// ============================================================

/// 自动检查并安装所有配置（Claude Code hooks + Codex OTel）
/// 供用户面向命令在启动时调用
pub fn auto_setup() {
    match ensure_claude_hooks() {
        Ok(true) => {
            eprintln!("{} 已自动注册 Claude Code hooks", "✓".green());
        }
        Ok(false) => {}
        Err(e) => {
            eprintln!("{} Claude Code hooks 自动注册失败: {}", "⚠".yellow(), e);
        }
    }

    match ensure_codex_config() {
        Ok(true) => {
            eprintln!("{} 已自动配置 Codex OTel + notify", "✓".green());
        }
        Ok(false) => {}
        Err(e) => {
            eprintln!("{} Codex 自动配置失败: {}", "⚠".yellow(), e);
        }
    }
}
