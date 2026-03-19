//! 事件监控 GUI 窗口。
//!
//! 职责与边界：
//! - 负责读取事件日志并用 `egui` 渲染实时监控界面。
//! - 负责管理 GUI 过滤器、自动滚动与窗口级国际化文案刷新。
//! - 不负责日志采集本身；事件写入由 logger/server/hook 模块完成。
//!
//! 关键副作用：
//! - 启动后台线程监控日志文件新增内容。
//! - 启动本地 OTel 接收服务器，以便 GUI 模式下也能接收 Codex 事件。
//! - 会在运行时更新窗口标题和界面文本。
//!
//! 关键依赖与约束：
//! - 依赖 `eframe/egui` 渲染界面，依赖 `crate::i18n` 提供翻译。
//! - 当前事件摘要文本直接使用日志内容，不做二次翻译。

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;

use crate::i18n::{self, AppLanguage, TranslationKey};
use crate::logger::{log_file_path, LogEntry};

/// 事件类型 → egui 颜色
fn event_color(event_type: &str) -> egui::Color32 {
    match event_type {
        "SessionStart" => egui::Color32::from_rgb(80, 200, 80),
        "SessionEnd" => egui::Color32::from_rgb(220, 60, 60),
        "PreToolUse" => egui::Color32::from_rgb(80, 200, 220),
        "PostToolUse" => egui::Color32::from_rgb(80, 130, 220),
        "PostToolUseFailure" => egui::Color32::from_rgb(220, 60, 60),
        "PermissionRequest" => egui::Color32::from_rgb(240, 200, 40),
        "Notification" => egui::Color32::from_rgb(220, 180, 40),
        "SubagentStart" => egui::Color32::from_rgb(180, 80, 220),
        "SubagentStop" => egui::Color32::from_rgb(180, 80, 220),
        "Stop" => egui::Color32::from_rgb(220, 60, 60),
        "UserPromptSubmit" => egui::Color32::from_rgb(80, 200, 80),
        "InstructionsLoaded" => egui::Color32::from_rgb(160, 160, 160),
        "TeammateIdle" => egui::Color32::from_rgb(220, 180, 40),
        "TaskCompleted" => egui::Color32::from_rgb(80, 200, 80),
        "ConfigChange" => egui::Color32::from_rgb(220, 180, 40),
        "WorktreeCreate" => egui::Color32::from_rgb(80, 200, 220),
        "WorktreeRemove" => egui::Color32::from_rgb(80, 200, 220),
        "PreCompact" => egui::Color32::from_rgb(160, 160, 160),
        "api_request" => egui::Color32::from_rgb(80, 200, 220),
        "tool_decision" => egui::Color32::from_rgb(80, 200, 220),
        "tool_result" => egui::Color32::from_rgb(80, 130, 220),
        "sse_event" => egui::Color32::from_rgb(160, 160, 160),
        _ => egui::Color32::from_rgb(200, 200, 200),
    }
}

/// 所有可能的事件类型（用于过滤器下拉）。
const EVENT_TYPES: &[&str] = &[
    "All",
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "InstructionsLoaded",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "SubagentStart",
    "SubagentStop",
    "Stop",
    "TeammateIdle",
    "TaskCompleted",
    "ConfigChange",
    "WorktreeCreate",
    "WorktreeRemove",
    "PreCompact",
    "api_request",
    "tool_decision",
    "tool_result",
    "sse_event",
];

/// 返回当前语言下的 GUI 事件类型显示名。
///
/// 入参：
/// - `language`: 当前界面使用的显示语言。
/// - `event_name`: 原始事件类型标识。
///
/// 返回值：
/// - 已知事件返回本地化标签，未知事件回退原始名称。
///
/// 错误处理：
/// - 不会失败；翻译模块内部会对未知事件做回退。
fn event_display_name(language: AppLanguage, event_name: &str) -> String {
    i18n::event_type_label(language, event_name)
}

/// GUI 应用状态。
///
/// 职责与边界：
/// - 保存当前已加载的日志条目、筛选条件与自动滚动状态。
/// - 不负责日志读取线程生命周期管理；日志加载由外部 helper 启动。
struct MonitorApp {
    entries: Arc<Mutex<Vec<LogEntry>>>,
    selected_filter: String,
    auto_scroll: bool,
}

impl MonitorApp {
    /// 创建 GUI 应用状态。
    ///
    /// 入参：
    /// - `entries`: 与日志监控线程共享的事件列表。
    ///
    /// 返回值：
    /// - 初始化后的 `MonitorApp`，默认显示全部事件并开启自动滚动。
    ///
    /// 错误处理：
    /// - 不会失败；该构造函数只做内存初始化。
    fn new(entries: Arc<Mutex<Vec<LogEntry>>>) -> Self {
        Self {
            entries,
            selected_filter: "All".to_string(),
            auto_scroll: true,
        }
    }
}

impl eframe::App for MonitorApp {
    /// 渲染 GUI 每一帧，并按当前语言刷新界面文案。
    ///
    /// 入参：
    /// - `ctx`: `egui` 上下文，用于请求重绘和更新窗口标题。
    /// - `_frame`: eframe 提供的窗口帧对象，本实现当前未直接使用。
    ///
    /// 返回值：
    /// - 无返回值；渲染结果直接写入当前帧。
    ///
    /// 错误处理：
    /// - 不返回 `Result`；锁竞争时会采用 poisoned lock 的内部值继续绘制。
    ///
    /// 关键副作用：
    /// - 每 500ms 请求一次重绘。
    /// - 会根据当前语言更新窗口标题，为未来运行时语言切换预留刷新能力。
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let language = i18n::current_language();

        // 请求持续重绘（用于实时更新）
        ctx.request_repaint_after(Duration::from_millis(500));
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(
            i18n::translate(language, TranslationKey::GuiWindowTitle).to_string(),
        ));

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(i18n::translate(language, TranslationKey::GuiFilterLabel));
                egui::ComboBox::from_id_salt("event_filter")
                    .selected_text(event_display_name(language, &self.selected_filter))
                    .show_ui(ui, |ui| {
                        for &name in EVENT_TYPES {
                            ui.selectable_value(
                                &mut self.selected_filter,
                                name.to_string(),
                                event_display_name(language, name),
                            );
                        }
                    });

                ui.separator();
                ui.checkbox(
                    &mut self.auto_scroll,
                    i18n::translate(language, TranslationKey::GuiAutoScroll),
                );

                ui.separator();
                if ui
                    .button(i18n::translate(language, TranslationKey::GuiClear))
                    .clicked()
                {
                    if let Ok(mut entries) = self.entries.lock() {
                        entries.clear();
                    }
                }

                ui.separator();
                if let Ok(entries) = self.entries.lock() {
                    ui.label(
                        egui::RichText::new(i18n::format_event_count(language, entries.len()))
                            .color(egui::Color32::from_rgb(160, 160, 160)),
                    );
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());

            // 过滤
            let filtered: Vec<&LogEntry> = entries
                .iter()
                .filter(|e| self.selected_filter == "All" || e.event_type == self.selected_filter)
                .collect();

            let text_style = egui::TextStyle::Monospace;
            let row_height = ui.text_style_height(&text_style) + 4.0;

            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .stick_to_bottom(self.auto_scroll)
                .show_rows(ui, row_height, filtered.len(), |ui, row_range| {
                    for idx in row_range {
                        if let Some(entry) = filtered.get(idx) {
                            ui.horizontal(|ui| {
                                // 时间
                                let time = if entry.timestamp.len() >= 19 {
                                    &entry.timestamp[11..19]
                                } else {
                                    &entry.timestamp
                                };
                                ui.label(
                                    egui::RichText::new(time)
                                        .monospace()
                                        .color(egui::Color32::from_rgb(120, 120, 120)),
                                );

                                // source 标签 (在时间之后，事件类型之前)
                                let source_color = match entry.source.as_str() {
                                    "cx" => egui::Color32::from_rgb(200, 100, 220), // 紫色
                                    _ => egui::Color32::from_rgb(80, 200, 220),     // 青色
                                };
                                let source_label = match entry.source.as_str() {
                                    "cx" => "CX",
                                    _ => "CC",
                                };
                                ui.label(
                                    egui::RichText::new(source_label)
                                        .monospace()
                                        .color(source_color),
                                );

                                // 事件类型
                                let color = event_color(&entry.event_type);
                                ui.label(
                                    egui::RichText::new(format!("{:>22}", entry.event_type))
                                        .monospace()
                                        .color(color),
                                );

                                // 工具名
                                if let Some(ref tool) = entry.tool_name {
                                    ui.label(
                                        egui::RichText::new(format!("[{}]", tool))
                                            .monospace()
                                            .color(egui::Color32::from_rgb(220, 180, 40)),
                                    );
                                }

                                // 摘要
                                ui.label(
                                    egui::RichText::new(&entry.summary)
                                        .monospace()
                                        .color(egui::Color32::from_rgb(220, 220, 220)),
                                );
                            });
                        }
                    }
                });
        });
    }
}

/// 后台线程：监控日志文件，将新事件推入共享列表
fn spawn_log_watcher(entries: Arc<Mutex<Vec<LogEntry>>>, filter: Option<String>) {
    thread::spawn(move || {
        let log_path = log_file_path();

        // 等待日志文件出现
        while !log_path.exists() {
            thread::sleep(Duration::from_secs(1));
        }

        // 先加载最近 50 条历史
        let file = File::open(&log_path).expect("Cannot open log file");
        let reader = BufReader::new(&file);
        let lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
        let start = if lines.len() > 50 {
            lines.len() - 50
        } else {
            0
        };

        {
            let mut locked = entries.lock().unwrap();
            for line in &lines[start..] {
                if let Ok(entry) = serde_json::from_str::<LogEntry>(line) {
                    if should_show(&entry, filter.as_deref()) {
                        locked.push(entry);
                    }
                }
            }
        }

        // 跟踪新事件
        let mut file = File::open(&log_path).expect("Cannot open log file");
        file.seek(SeekFrom::End(0)).expect("Cannot seek to end");
        let mut reader = BufReader::new(file);

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    thread::sleep(Duration::from_millis(200));
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        if let Ok(entry) = serde_json::from_str::<LogEntry>(trimmed) {
                            if should_show(&entry, filter.as_deref()) {
                                let mut locked = entries.lock().unwrap();
                                locked.push(entry);
                                // 保留最近 2000 条，防止内存无限增长
                                if locked.len() > 2000 {
                                    let drain_count = locked.len() - 2000;
                                    locked.drain(..drain_count);
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
}

/// 根据过滤器判断是否显示
fn should_show(entry: &LogEntry, filter: Option<&str>) -> bool {
    match filter {
        None => true,
        Some(f) => entry.event_type.to_lowercase().contains(&f.to_lowercase()),
    }
}

/// 启动事件监控 GUI 窗口。
///
/// 入参：
/// - `filter`: 可选的初始过滤关键词；若提供，则只显示事件类型中包含该关键词的日志。
///
/// 返回值：
/// - 无返回值；函数会阻塞当前线程直到 GUI 关闭。
///
/// 错误处理：
/// - 若运行时、窗口或 GUI 框架初始化失败，将直接 panic。
///
/// 关键副作用：
/// - 会启动后台 OTel 接收线程和日志监控线程。
/// - 会创建并显示始终置顶的 GUI 窗口。
pub fn run_gui(filter: Option<&str>) {
    // 自动启动 OTel 服务器（单线程 runtime，节省线程）
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Cannot create tokio runtime");
        rt.block_on(crate::server::run_server(4318));
    });

    let entries: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));

    // 启动后台日志监控线程
    spawn_log_watcher(entries.clone(), filter.map(|s| s.to_string()));

    // 配置并启动 egui 窗口
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([700.0, 450.0])
            .with_min_inner_size([400.0, 200.0])
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        i18n::translate(i18n::current_language(), TranslationKey::GuiWindowTitle),
        options,
        Box::new(move |cc| {
            // 加载中文字体（按平台选择系统字体）
            let mut fonts = egui::FontDefinitions::default();
            let font_path = if cfg!(target_os = "macos") {
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"
            } else if cfg!(target_os = "windows") {
                "C:\\Windows\\Fonts\\msyh.ttc"
            } else {
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc"
            };
            if let Ok(font_data) = std::fs::read(font_path) {
                fonts.font_data.insert(
                    "pingfang".to_owned(),
                    egui::FontData::from_owned(font_data).into(),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push("pingfang".to_owned());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push("pingfang".to_owned());
            }
            cc.egui_ctx.set_fonts(fonts);

            // 使用深色主题
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            // eframe 初始化会重置激活策略，需要再次隐藏 Dock 图标
            #[cfg(target_os = "macos")]
            crate::cat::hide_dock_icon();
            Ok(Box::new(MonitorApp::new(entries.clone())))
        }),
    )
    .expect("Failed to start GUI");
}
