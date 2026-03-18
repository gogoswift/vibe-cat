//! 应用国际化基础能力。
//!
//! 职责与边界：
//! - 负责封装应用语言枚举、系统语言解析、手动覆盖优先级与稳定翻译 key。
//! - 负责为各 UI 层提供统一的当前语言与文案查询入口。
//! - 不负责设置页、持久化配置、复杂复数规则或外部资源文件加载。
//!
//! 关键副作用：
//! - 在 macOS 上会读取系统首选语言列表以推断当前显示语言。
//! - 会读写进程内的语言覆盖状态，为未来手动切换预留入口。
//!
//! 关键依赖与约束：
//! - 默认语言固定为英文；仅支持英文与简体中文。
//! - 中文判定目前基于 locale 标识前缀，未知或读取失败时回退英文。

use std::sync::atomic::{AtomicU8, Ordering};

#[cfg(target_os = "macos")]
use objc2::msg_send;
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyClass, AnyObject};
#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(target_os = "macos")]
use std::os::raw::c_char;

const LANGUAGE_OVERRIDE_AUTO: u8 = 0;
const LANGUAGE_OVERRIDE_ENGLISH: u8 = 1;
const LANGUAGE_OVERRIDE_SIMPLIFIED_CHINESE: u8 = 2;

static LANGUAGE_OVERRIDE: AtomicU8 = AtomicU8::new(LANGUAGE_OVERRIDE_AUTO);

/// 应用当前支持的显示语言。
///
/// 语义与边界：
/// - 仅表示 UI 文案语言，不涉及区域格式、日期格式或数字格式。
/// - 当前仅支持英文与简体中文；未来若扩展更多语言，需要同步补充存储值与翻译映射。
///
/// 返回值：
/// - `English` 表示英文。
/// - `SimplifiedChinese` 表示简体中文。
///
/// 错误处理：
/// - 该枚举本身不承载错误；未知存储值会在解析时回退为 `None`，由上层继续走自动检测或默认值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppLanguage {
    English,
    SimplifiedChinese,
}

impl AppLanguage {
    /// 根据 locale 标识推断应用语言。
    ///
    /// 入参：
    /// - `locale_identifier`: 系统 locale 或语言标签，例如 `en-US`、`zh-Hans`、`zh-CN`。
    ///
    /// 返回值：
    /// - 命中 `zh` 前缀时返回 `SimplifiedChinese`，其它情况返回 `English`。
    ///
    /// 错误处理：
    /// - 空白或未知字符串不会抛错，统一按英文处理。
    ///
    /// 关键约束：
    /// - 当前产品只支持英文与简体中文，因此所有中文变体暂时都收敛到简体中文。
    fn from_locale_identifier(locale_identifier: &str) -> Self {
        let normalized = locale_identifier.trim().to_ascii_lowercase();
        if normalized.starts_with("zh") {
            Self::SimplifiedChinese
        } else {
            Self::English
        }
    }

    /// 将语言枚举编码为进程内原子存储值。
    ///
    /// 返回值：
    /// - 与 `LANGUAGE_OVERRIDE_*` 常量对应的稳定整数值。
    ///
    /// 错误处理：
    /// - 不会失败；所有支持的语言都有固定编码。
    #[allow(dead_code)]
    fn to_storage_value(self) -> u8 {
        match self {
            Self::English => LANGUAGE_OVERRIDE_ENGLISH,
            Self::SimplifiedChinese => LANGUAGE_OVERRIDE_SIMPLIFIED_CHINESE,
        }
    }

    /// 从进程内原子存储值恢复语言枚举。
    ///
    /// 入参：
    /// - `value`: 原子变量中保存的编码值。
    ///
    /// 返回值：
    /// - 编码合法时返回对应语言；未知值返回 `None`，交给上层继续走自动检测逻辑。
    ///
    /// 错误处理：
    /// - 对未知编码不 panic，直接返回 `None`。
    fn from_storage_value(value: u8) -> Option<Self> {
        match value {
            LANGUAGE_OVERRIDE_ENGLISH => Some(Self::English),
            LANGUAGE_OVERRIDE_SIMPLIFIED_CHINESE => Some(Self::SimplifiedChinese),
            _ => None,
        }
    }
}

/// 稳定的翻译 key。
///
/// 语义与边界：
/// - key 仅表达业务语义，不绑定具体语言文本。
/// - 当前覆盖托盘、GUI 与 CLI 帮助中已经接入国际化的文案。
///
/// 错误处理：
/// - 枚举本身不承载错误；新增 key 时必须同步扩展 `translate`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranslationKey {
    EventMonitor,
    Quit,
    DisplayLocation,
    Automatic,
    OnlyOneDisplayDetected,
    GuiWindowTitle,
    GuiFilterLabel,
    GuiAutoScroll,
    GuiClear,
    GuiEventSingular,
    GuiEventPlural,
    GuiEventCountChineseUnit,
    CliAppAbout,
    CliListenAbout,
    CliInstallAbout,
    CliInstallScopeHelp,
    CliTailAbout,
    CliFilterHelp,
    CliStatusAbout,
    CliGuiAbout,
    CliCatAbout,
    CliApproveAbout,
    CliMiniCatAbout,
    CliMiniCatAgentIdHelp,
    CliServerAbout,
    CliServerPortHelp,
}

/// 设置进程内语言覆盖值。
///
/// 入参：
/// - `language`: `Some` 时强制使用指定语言；`None` 时清除覆盖并恢复自动检测。
///
/// 返回值：
/// - 无返回值；结果体现在后续 `current_language()` 的解析结果中。
///
/// 错误处理：
/// - 不会抛错；该函数只更新进程内原子状态。
///
/// 关键副作用：
/// - 修改全局语言覆盖状态，会影响后续所有通过 `current_language()` 获取文案的 UI。
#[allow(dead_code)]
pub fn set_language_override(language: Option<AppLanguage>) {
    let value = language.map_or(LANGUAGE_OVERRIDE_AUTO, AppLanguage::to_storage_value);
    LANGUAGE_OVERRIDE.store(value, Ordering::Relaxed);
}

/// 返回当前生效的应用语言。
///
/// 语义与边界：
/// - 先读取手动覆盖值；未设置时再自动探测系统语言；仍失败则回退英文。
/// - 该函数只负责解析“应显示哪种文案”，不负责刷新具体 UI 控件。
///
/// 返回值：
/// - 当前应该展示的 `AppLanguage`。
///
/// 错误处理：
/// - 系统语言读取失败或不可用时不会报错，直接回退英文。
pub fn current_language() -> AppLanguage {
    resolve_language(
        detect_system_locale_identifier().as_deref(),
        language_override(),
    )
}

/// 根据翻译 key 返回指定语言的文案。
///
/// 入参：
/// - `language`: 目标语言。
/// - `key`: 语义稳定的翻译 key。
///
/// 返回值：
/// - 对应语言下的静态文案字符串。
///
/// 错误处理：
/// - 不会失败；当前所有已定义 key 都必须在此处完整映射。
pub fn translate(language: AppLanguage, key: TranslationKey) -> &'static str {
    match (language, key) {
        (AppLanguage::English, TranslationKey::EventMonitor) => "Event Monitor",
        (AppLanguage::SimplifiedChinese, TranslationKey::EventMonitor) => "事件监控",
        (AppLanguage::English, TranslationKey::Quit) => "Quit",
        (AppLanguage::SimplifiedChinese, TranslationKey::Quit) => "退出",
        (AppLanguage::English, TranslationKey::DisplayLocation) => "Display Location",
        (AppLanguage::SimplifiedChinese, TranslationKey::DisplayLocation) => "显示位置",
        (AppLanguage::English, TranslationKey::Automatic) => "Automatic",
        (AppLanguage::SimplifiedChinese, TranslationKey::Automatic) => "自动",
        (AppLanguage::English, TranslationKey::OnlyOneDisplayDetected) => {
            "Only one display detected"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::OnlyOneDisplayDetected) => {
            "当前仅检测到一个显示器"
        }
        (AppLanguage::English, TranslationKey::GuiWindowTitle) => "VibeCat Event Monitor",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiWindowTitle) => "VibeCat 事件监控",
        (AppLanguage::English, TranslationKey::GuiFilterLabel) => "Filter:",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiFilterLabel) => "筛选：",
        (AppLanguage::English, TranslationKey::GuiAutoScroll) => "Auto-scroll",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiAutoScroll) => "自动滚动",
        (AppLanguage::English, TranslationKey::GuiClear) => "Clear",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiClear) => "清空",
        (AppLanguage::English, TranslationKey::GuiEventSingular) => "event",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiEventSingular) => "条事件",
        (AppLanguage::English, TranslationKey::GuiEventPlural) => "events",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiEventPlural) => "条事件",
        (AppLanguage::English, TranslationKey::GuiEventCountChineseUnit) => "events",
        (AppLanguage::SimplifiedChinese, TranslationKey::GuiEventCountChineseUnit) => "条事件",
        (AppLanguage::English, TranslationKey::CliAppAbout) => {
            "Passive hook event monitor for Claude Code"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliAppAbout) => {
            "Claude Code Hook 事件监听器 - 被动记录所有 hook 事件"
        }
        (AppLanguage::English, TranslationKey::CliListenAbout) => {
            "Listen for hook events from stdin"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliListenAbout) => {
            "监听 hook 事件（被 Claude Code 调用，从 stdin 读取 JSON）"
        }
        (AppLanguage::English, TranslationKey::CliInstallAbout) => {
            "Install hook configuration into settings.json"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliInstallAbout) => {
            "安装 hook 配置到 settings.json"
        }
        (AppLanguage::English, TranslationKey::CliInstallScopeHelp) => {
            "Installation scope: user (global) or project"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliInstallScopeHelp) => {
            "安装范围：user（全局）或 project（项目级别）"
        }
        (AppLanguage::English, TranslationKey::CliTailAbout) => "Stream event logs in real time",
        (AppLanguage::SimplifiedChinese, TranslationKey::CliTailAbout) => {
            "实时查看事件日志（类似 tail -f）"
        }
        (AppLanguage::English, TranslationKey::CliFilterHelp) => {
            "Filter by event type, for example PreToolUse or Stop"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliFilterHelp) => {
            "按事件类型过滤（如 PreToolUse、Stop 等）"
        }
        (AppLanguage::English, TranslationKey::CliStatusAbout) => {
            "Show a summary of recent event status"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliStatusAbout) => {
            "显示最近事件的状态摘要"
        }
        (AppLanguage::English, TranslationKey::CliGuiAbout) => {
            "Open a topmost window for event logs"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliGuiAbout) => "打开置顶窗口显示事件日志",
        (AppLanguage::English, TranslationKey::CliCatAbout) => "Launch the desktop pet cat",
        (AppLanguage::SimplifiedChinese, TranslationKey::CliCatAbout) => "启动桌面宠物猫",
        (AppLanguage::English, TranslationKey::CliApproveAbout) => {
            "Handle PermissionRequest confirmation"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliApproveAbout) => {
            "处理 PermissionRequest 确认（被 hook 调用）"
        }
        (AppLanguage::English, TranslationKey::CliMiniCatAbout) => {
            "Launch a mini cat for subagent events and exit automatically"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliMiniCatAbout) => {
            "启动迷你猫（被 subagent 触发，自动退出）"
        }
        (AppLanguage::English, TranslationKey::CliMiniCatAgentIdHelp) => {
            "Subagent agent_id used to match SubagentStop"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliMiniCatAgentIdHelp) => {
            "subagent 的 agent_id（用于匹配 SubagentStop）"
        }
        (AppLanguage::English, TranslationKey::CliServerAbout) => {
            "Start the Codex OTel HTTP receiver"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliServerAbout) => {
            "启动 Codex OTel HTTP 接收服务器"
        }
        (AppLanguage::English, TranslationKey::CliServerPortHelp) => {
            "Listening port, defaults to 4318"
        }
        (AppLanguage::SimplifiedChinese, TranslationKey::CliServerPortHelp) => {
            "监听端口（默认 4318）"
        }
    }
}

/// 返回 GUI 过滤器中某个事件类型的显示标签。
///
/// 入参：
/// - `language`: 目标显示语言。
/// - `event_type`: 原始事件类型标识，例如 `PermissionRequest` 或 `tool_result`。
///
/// 返回值：
/// - 已知事件类型返回对应语言下的用户可读标签。
/// - 未知事件类型返回原始事件名，避免 UI 丢失信息。
///
/// 错误处理：
/// - 不会失败；未知事件名静默回退原始值。
pub fn event_type_label(language: AppLanguage, event_type: &str) -> String {
    let translated = match (language, event_type) {
        (AppLanguage::English, "All") => Some("All"),
        (AppLanguage::SimplifiedChinese, "All") => Some("全部"),
        (AppLanguage::English, "SessionStart") => Some("Session Start"),
        (AppLanguage::SimplifiedChinese, "SessionStart") => Some("会话开始"),
        (AppLanguage::English, "SessionEnd") => Some("Session End"),
        (AppLanguage::SimplifiedChinese, "SessionEnd") => Some("会话结束"),
        (AppLanguage::English, "UserPromptSubmit") => Some("User Prompt Submit"),
        (AppLanguage::SimplifiedChinese, "UserPromptSubmit") => Some("用户提交"),
        (AppLanguage::English, "InstructionsLoaded") => Some("Instructions Loaded"),
        (AppLanguage::SimplifiedChinese, "InstructionsLoaded") => Some("指令加载"),
        (AppLanguage::English, "PreToolUse") => Some("Pre Tool Use"),
        (AppLanguage::SimplifiedChinese, "PreToolUse") => Some("工具调用前"),
        (AppLanguage::English, "PostToolUse") => Some("Post Tool Use"),
        (AppLanguage::SimplifiedChinese, "PostToolUse") => Some("工具调用后"),
        (AppLanguage::English, "PostToolUseFailure") => Some("Post Tool Use Failure"),
        (AppLanguage::SimplifiedChinese, "PostToolUseFailure") => Some("工具调用失败"),
        (AppLanguage::English, "PermissionRequest") => Some("Permission Request"),
        (AppLanguage::SimplifiedChinese, "PermissionRequest") => Some("权限请求"),
        (AppLanguage::English, "Notification") => Some("Notification"),
        (AppLanguage::SimplifiedChinese, "Notification") => Some("通知"),
        (AppLanguage::English, "SubagentStart") => Some("Subagent Start"),
        (AppLanguage::SimplifiedChinese, "SubagentStart") => Some("子代理启动"),
        (AppLanguage::English, "SubagentStop") => Some("Subagent Stop"),
        (AppLanguage::SimplifiedChinese, "SubagentStop") => Some("子代理停止"),
        (AppLanguage::English, "Stop") => Some("Stop"),
        (AppLanguage::SimplifiedChinese, "Stop") => Some("停止"),
        (AppLanguage::English, "TeammateIdle") => Some("Teammate Idle"),
        (AppLanguage::SimplifiedChinese, "TeammateIdle") => Some("队友空闲"),
        (AppLanguage::English, "TaskCompleted") => Some("Task Completed"),
        (AppLanguage::SimplifiedChinese, "TaskCompleted") => Some("任务完成"),
        (AppLanguage::English, "ConfigChange") => Some("Config Change"),
        (AppLanguage::SimplifiedChinese, "ConfigChange") => Some("配置变更"),
        (AppLanguage::English, "WorktreeCreate") => Some("Worktree Create"),
        (AppLanguage::SimplifiedChinese, "WorktreeCreate") => Some("工作树创建"),
        (AppLanguage::English, "WorktreeRemove") => Some("Worktree Remove"),
        (AppLanguage::SimplifiedChinese, "WorktreeRemove") => Some("工作树删除"),
        (AppLanguage::English, "PreCompact") => Some("Pre Compact"),
        (AppLanguage::SimplifiedChinese, "PreCompact") => Some("压缩前"),
        (AppLanguage::English, "api_request") => Some("CX API Request"),
        (AppLanguage::SimplifiedChinese, "api_request") => Some("CX API请求"),
        (AppLanguage::English, "tool_decision") => Some("CX Tool Decision"),
        (AppLanguage::SimplifiedChinese, "tool_decision") => Some("CX工具决策"),
        (AppLanguage::English, "tool_result") => Some("CX Tool Result"),
        (AppLanguage::SimplifiedChinese, "tool_result") => Some("CX工具结果"),
        (AppLanguage::English, "sse_event") => Some("CX SSE Event"),
        (AppLanguage::SimplifiedChinese, "sse_event") => Some("CX SSE事件"),
        _ => None,
    };

    translated.unwrap_or(event_type).to_string()
}

/// 按当前语言格式化 GUI 顶部的事件数量文案。
///
/// 入参：
/// - `language`: 目标显示语言。
/// - `count`: 当前事件条数。
///
/// 返回值：
/// - 英文下返回 `N event` / `N events`。
/// - 简体中文下返回 `N 条事件`。
///
/// 错误处理：
/// - 不会失败；所有计数都直接格式化为字符串。
pub fn format_event_count(language: AppLanguage, count: usize) -> String {
    match language {
        AppLanguage::English => {
            let unit = if count == 1 {
                translate(language, TranslationKey::GuiEventSingular)
            } else {
                translate(language, TranslationKey::GuiEventPlural)
            };
            format!("{count} {unit}")
        }
        AppLanguage::SimplifiedChinese => format!(
            "{count} {}",
            translate(language, TranslationKey::GuiEventCountChineseUnit)
        ),
    }
}

/// 读取当前进程内的语言覆盖值。
///
/// 返回值：
/// - 若未来设置页已指定语言，则返回对应 `Some(AppLanguage)`。
/// - 若仍处于自动模式，则返回 `None`。
///
/// 错误处理：
/// - 遇到未知存储值时不会 panic，而是按自动模式处理。
fn language_override() -> Option<AppLanguage> {
    AppLanguage::from_storage_value(LANGUAGE_OVERRIDE.load(Ordering::Relaxed))
}

/// 按“手动覆盖优先，其次 locale 自动探测，最后英文回退”的顺序解析语言。
///
/// 入参：
/// - `locale_identifier`: 系统探测到的语言标签，可能为空。
/// - `override_language`: 手动覆盖语言，若存在则优先级最高。
///
/// 返回值：
/// - 最终用于展示文案的语言。
///
/// 错误处理：
/// - 缺失或未知 locale 不会报错，统一回退英文。
fn resolve_language(
    locale_identifier: Option<&str>,
    override_language: Option<AppLanguage>,
) -> AppLanguage {
    if let Some(language) = override_language {
        return language;
    }

    locale_identifier
        .map(AppLanguage::from_locale_identifier)
        .unwrap_or(AppLanguage::English)
}

/// 读取系统首选语言标识。
///
/// 返回值：
/// - 成功时返回首选语言标签，例如 `en-US` 或 `zh-Hans-CN`。
/// - 失败时返回 `None`。
///
/// 错误处理：
/// - macOS API 读取失败或返回空值时，不抛错，回退到环境变量探测。
fn detect_system_locale_identifier() -> Option<String> {
    detect_macos_locale_identifier().or_else(detect_env_locale_identifier)
}

/// 在 macOS 上从系统首选语言列表读取首项语言标签。
///
/// 返回值：
/// - 返回 `NSLocale.preferredLanguages` 的第一项字符串。
///
/// 错误处理：
/// - 任何 Objective-C 调用失败、空指针或非法 UTF-8 都返回 `None`。
///
/// 关键副作用：
/// - 会读取系统全局语言设置，但不会修改系统状态。
#[cfg(target_os = "macos")]
fn detect_macos_locale_identifier() -> Option<String> {
    unsafe {
        let locale_class = AnyClass::get("NSLocale")?;
        let preferred_languages: *mut AnyObject = msg_send![locale_class, preferredLanguages];
        if preferred_languages.is_null() {
            return None;
        }

        let count: usize = msg_send![preferred_languages, count];
        if count == 0 {
            return None;
        }

        let first_language: *mut AnyObject = msg_send![preferred_languages, objectAtIndex: 0usize];
        if first_language.is_null() {
            return None;
        }

        let utf8_ptr: *const c_char = msg_send![first_language, UTF8String];
        if utf8_ptr.is_null() {
            return None;
        }

        CStr::from_ptr(utf8_ptr)
            .to_str()
            .ok()
            .map(|value| value.to_string())
    }
}

/// Windows: 通过 GetUserDefaultLocaleName 获取系统语言
#[cfg(target_os = "windows")]
fn detect_macos_locale_identifier() -> Option<String> {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;
    let mut buf = [0u16; 85]; // LOCALE_NAME_MAX_LENGTH
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if len > 0 {
        String::from_utf16(&buf[..len as usize - 1]).ok()
    } else {
        None
    }
}

/// 非 macOS/Windows 平台空实现，走环境变量回退。
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn detect_macos_locale_identifier() -> Option<String> {
    None
}

/// 从环境变量读取 locale 作为系统语言探测的兜底来源。
///
/// 返回值：
/// - 优先返回 `LC_ALL`，其次 `LANG`，并去掉编码后缀如 `.UTF-8`。
/// - 没有可用值时返回 `None`。
///
/// 错误处理：
/// - 变量缺失或为空时静默返回 `None`。
fn detect_env_locale_identifier() -> Option<String> {
    ["LC_ALL", "LANG"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok())
        .map(|value| {
            value
                .split('.')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{
        event_type_label, format_event_count, resolve_language, translate, AppLanguage,
        TranslationKey,
    };

    #[test]
    fn locale_parser_defaults_to_english_for_non_chinese_locale() {
        assert_eq!(resolve_language(Some("en-US"), None), AppLanguage::English);
    }

    #[test]
    fn locale_parser_maps_chinese_variants_to_simplified_chinese() {
        assert_eq!(
            resolve_language(Some("zh-Hans"), None),
            AppLanguage::SimplifiedChinese
        );
        assert_eq!(
            resolve_language(Some("zh-CN"), None),
            AppLanguage::SimplifiedChinese
        );
    }

    #[test]
    fn resolved_language_prefers_manual_override_over_detected_locale() {
        assert_eq!(
            resolve_language(Some("zh-CN"), Some(AppLanguage::English)),
            AppLanguage::English
        );
    }

    #[test]
    fn resolve_from_missing_locale_falls_back_to_english() {
        assert_eq!(resolve_language(None, None), AppLanguage::English);
    }

    #[test]
    fn translate_returns_expected_labels_for_tray_keys() {
        assert_eq!(
            translate(AppLanguage::English, TranslationKey::EventMonitor),
            "Event Monitor"
        );
        assert_eq!(
            translate(AppLanguage::SimplifiedChinese, TranslationKey::EventMonitor,),
            "事件监控"
        );
        assert_eq!(
            translate(AppLanguage::English, TranslationKey::Quit),
            "Quit"
        );
        assert_eq!(
            translate(AppLanguage::SimplifiedChinese, TranslationKey::Quit),
            "退出"
        );
    }

    #[test]
    fn translate_returns_expected_labels_for_gui_keys() {
        assert_eq!(
            translate(AppLanguage::English, TranslationKey::GuiWindowTitle),
            "VibeCat Event Monitor"
        );
        assert_eq!(
            translate(
                AppLanguage::SimplifiedChinese,
                TranslationKey::GuiWindowTitle
            ),
            "VibeCat 事件监控"
        );
        assert_eq!(
            translate(AppLanguage::English, TranslationKey::GuiFilterLabel),
            "Filter:"
        );
        assert_eq!(
            translate(
                AppLanguage::SimplifiedChinese,
                TranslationKey::GuiFilterLabel
            ),
            "筛选："
        );
        assert_eq!(
            translate(AppLanguage::English, TranslationKey::GuiAutoScroll),
            "Auto-scroll"
        );
        assert_eq!(
            translate(
                AppLanguage::SimplifiedChinese,
                TranslationKey::GuiAutoScroll,
            ),
            "自动滚动"
        );
        assert_eq!(
            translate(AppLanguage::English, TranslationKey::GuiClear),
            "Clear"
        );
        assert_eq!(
            translate(AppLanguage::SimplifiedChinese, TranslationKey::GuiClear),
            "清空"
        );
    }

    #[test]
    fn event_type_labels_follow_language() {
        assert_eq!(
            event_type_label(AppLanguage::English, "PermissionRequest"),
            "Permission Request"
        );
        assert_eq!(
            event_type_label(AppLanguage::SimplifiedChinese, "PermissionRequest"),
            "权限请求"
        );
        assert_eq!(event_type_label(AppLanguage::English, "unknown"), "unknown");
    }

    #[test]
    fn format_event_count_uses_language_specific_copy() {
        assert_eq!(format_event_count(AppLanguage::English, 1), "1 event");
        assert_eq!(format_event_count(AppLanguage::English, 3), "3 events");
        assert_eq!(
            format_event_count(AppLanguage::SimplifiedChinese, 3),
            "3 条事件"
        );
    }
}
