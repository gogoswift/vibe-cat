//! 应用命令行与运行入口。
//!
//! 职责与边界：
//! - 负责解析 CLI 参数，并把命令分发到安装、日志、GUI 与猫窗口等模块。
//! - 负责协调进程级启动流程，例如单实例检查、GUI/Tray 启动与命令出口码。
//! - 不负责具体业务实现；各命令的核心逻辑由对应子模块承载。
//!
//! 关键副作用：
//! - 读写用户目录下的配置/日志文件。
//! - 启动 GUI、HTTP 服务与系统托盘集成。
//! - 输出终端日志并设置进程退出状态。
//!
//! 关键依赖与约束：
//! - 依赖 `clap` 解析参数，依赖各子模块提供命令实现。
//! - macOS 相关 GUI/Tray 逻辑仅在对应平台能力可用时生效。

mod approve;
mod cat;
mod cat_layout;
mod events;
mod gui;
mod i18n;
mod installer;
mod logger;
mod server;
mod tail;
#[cfg(target_os = "macos")]
mod tray;

use std::io::Read;

use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use colored::*;

/// 命令行入口参数。
///
/// 职责与边界：
/// - 仅描述顶层 CLI 结构，并把实际命令解析委托给 clap。
/// - 不负责根据语言动态改写帮助文案；该行为由运行时 builder 完成。
#[derive(Parser)]
#[command(
    name = "vibe-cat",
    about = "Claude Code Hook 事件监听器 - 被动记录所有 hook 事件",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

/// 应用支持的子命令集合。
///
/// 职责与边界：
/// - 定义命令名、参数结构和默认解析规则。
/// - 不负责命令帮助的国际化文案；帮助文本会在运行时统一覆盖。
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
            Commands::Gui { .. }
                | Commands::Tail { .. }
                | Commands::Status
                | Commands::Cat
                | Commands::Server { .. }
        )
    }
}

/// 构建当前语言下的 clap 命令定义。
///
/// 入参：
/// - `language`: 当前应显示的帮助文案语言。
///
/// 返回值：
/// - 已经覆盖根命令、子命令和参数帮助文本的 `clap::Command`。
///
/// 错误处理：
/// - 不返回 `Result`；命令结构异常会在 clap 内部后续使用时暴露。
///
/// 关键副作用：
/// - 无直接副作用；仅构建命令定义对象。
fn build_localized_cli_command(language: i18n::AppLanguage) -> clap::Command {
    Cli::command()
        .about(i18n::translate(language, i18n::TranslationKey::CliAppAbout))
        .long_about(i18n::translate(language, i18n::TranslationKey::CliAppAbout))
        .mut_subcommand("listen", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliListenAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliListenAbout,
                ))
        })
        .mut_subcommand("install", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliInstallAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliInstallAbout,
                ))
                .mut_arg("scope", |arg| {
                    arg.help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliInstallScopeHelp,
                    ))
                    .long_help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliInstallScopeHelp,
                    ))
                })
        })
        .mut_subcommand("tail", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliTailAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliTailAbout,
                ))
                .mut_arg("filter", |arg| {
                    arg.help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliFilterHelp,
                    ))
                    .long_help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliFilterHelp,
                    ))
                })
        })
        .mut_subcommand("status", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliStatusAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliStatusAbout,
                ))
        })
        .mut_subcommand("gui", |subcommand| {
            subcommand
                .about(i18n::translate(language, i18n::TranslationKey::CliGuiAbout))
                .long_about(i18n::translate(language, i18n::TranslationKey::CliGuiAbout))
                .mut_arg("filter", |arg| {
                    arg.help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliFilterHelp,
                    ))
                    .long_help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliFilterHelp,
                    ))
                })
        })
        .mut_subcommand("cat", |subcommand| {
            subcommand
                .about(i18n::translate(language, i18n::TranslationKey::CliCatAbout))
                .long_about(i18n::translate(language, i18n::TranslationKey::CliCatAbout))
        })
        .mut_subcommand("approve", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliApproveAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliApproveAbout,
                ))
        })
        .mut_subcommand("mini-cat", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliMiniCatAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliMiniCatAbout,
                ))
                .mut_arg("agent_id", |arg| {
                    arg.help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliMiniCatAgentIdHelp,
                    ))
                    .long_help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliMiniCatAgentIdHelp,
                    ))
                })
        })
        .mut_subcommand("server", |subcommand| {
            subcommand
                .about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliServerAbout,
                ))
                .long_about(i18n::translate(
                    language,
                    i18n::TranslationKey::CliServerAbout,
                ))
                .mut_arg("port", |arg| {
                    arg.help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliServerPortHelp,
                    ))
                    .long_help(i18n::translate(
                        language,
                        i18n::TranslationKey::CliServerPortHelp,
                    ))
                })
        })
}

/// 使用当前语言解析命令行参数。
///
/// 返回值：
/// - 已完成解析的 `Cli` 结构。
///
/// 错误处理：
/// - 参数校验失败或用户请求帮助/版本时，clap 会自行输出信息并退出进程。
fn parse_cli() -> Cli {
    let language = i18n::current_language();
    let matches = build_localized_cli_command(language).get_matches();
    Cli::from_arg_matches(&matches).unwrap_or_else(|err| err.exit())
}

/// 应用的实际进程入口。
///
/// 职责与边界：
/// - 负责完成启动期平台初始化、解析命令行参数并分发到具体命令处理函数。
/// - 不负责子命令业务实现；具体逻辑由各模块函数承载。
///
/// 关键副作用：
/// - 可能修改 macOS 运行时 UI 行为，启动自动配置、GUI 和后台服务。
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

    let cli = parse_cli();
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
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Cannot create tokio runtime");
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
            println!("  2. Run: vibe-cat tail");
            println!("  3. Use Claude Code normally and watch events flow");
        }
        Err(e) => {
            eprintln!("{}: {}", "Installation failed".red().bold(), e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::i18n::AppLanguage;

    use super::build_localized_cli_command;

    #[test]
    fn cli_help_english_root_help_contains_localized_descriptions() {
        let mut command = build_localized_cli_command(AppLanguage::English);
        let help = command.render_help().to_string();

        assert!(help.contains("Passive hook event monitor for Claude Code"));
        assert!(help.contains("Open a topmost window for event logs"));
    }

    #[test]
    fn cli_help_chinese_install_help_contains_localized_argument_help() {
        let mut command = build_localized_cli_command(AppLanguage::SimplifiedChinese);
        let install = command
            .find_subcommand_mut("install")
            .expect("install command");
        let help = install.render_help().to_string();

        assert!(help.contains("安装 hook 配置到 settings.json"));
        assert!(help.contains("安装范围：user（全局）或 project（项目级别）"));
    }
}
