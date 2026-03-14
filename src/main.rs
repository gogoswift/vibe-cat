mod approve;
mod cat;
mod events;
mod gui;
mod installer;
mod logger;
mod server;
mod tail;
#[cfg(target_os = "macos")]
mod tray;

use std::io::Read;

use clap::{Parser, Subcommand};
use colored::*;

#[derive(Parser)]
#[command(
    name = "claude-hook-monitor",
    about = "Claude Code Hook 事件监听器 - 被动记录所有 hook 事件",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 监听 hook 事件（被 Claude Code 调用，从 stdin 读取 JSON）
    Listen,

    /// 安装 hook 配置到 settings.json
    Install {
        /// 安装范围：user（全局）或 project（项目级别）
        #[arg(long, default_value = "user")]
        scope: String,
    },

    /// 实时查看事件日志（类似 tail -f）
    Tail {
        /// 按事件类型过滤（如 PreToolUse, Stop 等）
        #[arg(long)]
        filter: Option<String>,
    },

    /// 显示最近事件的状态摘要
    Status,

    /// 打开置顶窗口显示事件日志
    Gui {
        /// 按事件类型过滤（如 PreToolUse, Stop 等）
        #[arg(long)]
        filter: Option<String>,
    },

    /// 启动桌面宠物猫
    Cat,

    /// 处理 PermissionRequest 确认（被 hook 调用）
    Approve,

    /// 启动迷你猫（被 subagent 触发，自动退出）
    MiniCat {
        /// subagent 的 agent_id（用于匹配 SubagentStop）
        #[arg(long)]
        agent_id: String,
    },

    /// 启动 Codex OTel HTTP 接收服务器
    Server {
        /// 监听端口（默认 4318）
        #[arg(long, default_value = "4318")]
        port: u16,
    },

}

impl Commands {
    /// 是否需要在启动时自动检查并安装 hook 配置
    fn needs_auto_setup(&self) -> bool {
        matches!(
            self,
            Commands::Gui { .. } | Commands::Tail { .. } | Commands::Status | Commands::Cat | Commands::Server { .. }
        )
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    cat::hide_dock_icon();

    // 禁用 macOS AutoFill，防止系统为 eframe 窗口派生大量 AutoFill XPC 进程
    #[cfg(target_os = "macos")]
    {
        use objc2::runtime::AnyObject;
        unsafe {
            let cls = objc2::runtime::AnyClass::get("NSUserDefaults").unwrap();
            let defaults: *mut AnyObject = objc2::msg_send![cls, standardUserDefaults];
            let key: *mut AnyObject = objc2::msg_send![
                objc2::runtime::AnyClass::get("NSString").unwrap(),
                stringWithUTF8String: c"AutoFillEnabled".as_ptr()
            ];
            let _: () = objc2::msg_send![defaults, setBool: false forKey: key];
        }
    }

    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Commands::Cat);

    if command.needs_auto_setup() {
        installer::auto_setup();
    }

    match command {
        Commands::Listen => handle_listen(),
        Commands::Install { scope } => handle_install(&scope),
        Commands::Tail { filter } => tail::tail_log(filter.as_deref()),
        Commands::Status => tail::show_status(),
        Commands::Gui { filter } => {
            gui::run_gui(filter.as_deref());
        }
        Commands::Cat => cat::run_cat(),
        Commands::Approve => approve::handle_approve(),
        Commands::MiniCat { agent_id: _ } => {
            eprintln!("MiniCat command is deprecated. Mini cats are now managed within the unified cat window.");
        }
        Commands::Server { port } => {
            let rt = tokio::runtime::Runtime::new().expect("Cannot create tokio runtime");
            rt.block_on(server::run_server(port));
        }
    }
}

/// 处理 listen 命令：从 stdin 读取 JSON，写入日志
fn handle_listen() {
    // 从 stdin 读取全部内容
    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("{}: {}", "Failed to read stdin".red(), e);
        // 始终 exit 0，不干扰 Claude Code
        return;
    }

    if input.trim().is_empty() {
        eprintln!("{}", "Empty stdin input".yellow());
        return;
    }

    // 先解析为通用 Value（保留原始数据）
    let raw_json: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}: {}", "Failed to parse JSON".red(), e);
            return;
        }
    };

    // 解析为具体事件类型
    let event: events::HookEvent = match serde_json::from_str(&input) {
        Ok(e) => e,
        Err(e) => {
            // 如果无法解析为已知事件，仍然记录原始 JSON
            eprintln!(
                "{}: {} (event: {:?})",
                "Unknown event type".yellow(),
                e,
                raw_json.get("hook_event_name")
            );
            // 尝试写入原始 JSON 作为 fallback
            if let Err(write_err) = write_raw_event(&raw_json) {
                eprintln!("{}: {}", "Failed to write log".red(), write_err);
            }
            return;
        }
    };

    // 写入日志文件
    if let Err(e) = logger::write_event(&event, &raw_json) {
        eprintln!("{}: {}", "Failed to write log".red(), e);
        return;
    }

    // stderr 输出彩色摘要（调试用，不影响 Claude Code）
    let event_type = event.event_type();
    let summary = event.summary();
    eprintln!(
        "{} {} {}",
        chrono::Local::now().format("%H:%M:%S").to_string().dimmed(),
        format!("[{}]", event_type).cyan(),
        summary
    );

    // 重要：stdout 不输出任何内容！
    // exit 0（函数正常返回即 exit 0）
}

/// 将无法解析的事件作为原始 JSON 写入日志
fn write_raw_event(raw: &serde_json::Value) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;

    logger::ensure_log_dir()?;

    let entry = serde_json::json!({
        "timestamp": chrono::Local::now().to_rfc3339(),
        "source": "cc",
        "event_type": raw.get("hook_event_name").and_then(|v| v.as_str()).unwrap_or("Unknown"),
        "session_id": raw.get("session_id").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "summary": format!("Unknown event: {}", raw.get("hook_event_name").and_then(|v| v.as_str()).unwrap_or("?")),
        "raw": raw,
    });

    let mut line = serde_json::to_string(&entry)?;
    line.push('\n');

    let log_path = logger::log_file_path();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;

    use fs2::FileExt;
    file.lock_exclusive()?;
    file.write_all(line.as_bytes())?;
    file.unlock()?;

    Ok(())
}

/// 处理 install 命令
fn handle_install(scope: &str) {
    let install_scope = match scope {
        "user" => installer::InstallScope::User,
        "project" => installer::InstallScope::Project,
        _ => {
            eprintln!(
                "{}: scope must be 'user' or 'project'",
                "Error".red().bold()
            );
            std::process::exit(1);
        }
    };

    match installer::install(install_scope) {
        Ok(msg) => {
            println!(
                "{}",
                "Hook configuration installed successfully!".green().bold()
            );
            println!("{}", msg);
            println!("\n{}", "Next steps:".bold());
            println!("  1. Restart Claude Code to load new hooks");
            println!("  2. Run: claude-hook-monitor tail");
            println!("  3. Use Claude Code normally and watch events flow");
        }
        Err(e) => {
            eprintln!("{}: {}", "Installation failed".red().bold(), e);
            std::process::exit(1);
        }
    }
}
