use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui;

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
        _ => egui::Color32::from_rgb(200, 200, 200),
    }
}

/// 所有可能的事件类型（用于过滤器下拉）：(事件名, 中文说明)
const EVENT_TYPES: &[(&str, &str)] = &[
    ("All", "全部"),
    ("SessionStart", "会话开始"),
    ("SessionEnd", "会话结束"),
    ("UserPromptSubmit", "用户提交"),
    ("InstructionsLoaded", "指令加载"),
    ("PreToolUse", "工具调用前"),
    ("PostToolUse", "工具调用后"),
    ("PostToolUseFailure", "工具调用失败"),
    ("PermissionRequest", "权限请求"),
    ("Notification", "通知"),
    ("SubagentStart", "子代理启动"),
    ("SubagentStop", "子代理停止"),
    ("Stop", "停止"),
    ("TeammateIdle", "队友空闲"),
    ("TaskCompleted", "任务完成"),
    ("ConfigChange", "配置变更"),
    ("WorktreeCreate", "工作树创建"),
    ("WorktreeRemove", "工作树删除"),
    ("PreCompact", "压缩前"),
];

/// 根据事件名查找显示文本
fn event_display_name(event_name: &str) -> String {
    EVENT_TYPES
        .iter()
        .find(|(name, _)| *name == event_name)
        .map(|(name, cn)| format!("{} [{}]", name, cn))
        .unwrap_or_else(|| event_name.to_string())
}

/// GUI 应用状态
struct MonitorApp {
    entries: Arc<Mutex<Vec<LogEntry>>>,
    selected_filter: String,
    auto_scroll: bool,
}

impl MonitorApp {
    fn new(entries: Arc<Mutex<Vec<LogEntry>>>) -> Self {
        Self {
            entries,
            selected_filter: "All".to_string(),
            auto_scroll: true,
        }
    }
}

impl eframe::App for MonitorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 请求持续重绘（用于实时更新）
        ctx.request_repaint_after(Duration::from_millis(500));

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Filter:");
                egui::ComboBox::from_id_salt("event_filter")
                    .selected_text(event_display_name(&self.selected_filter))
                    .show_ui(ui, |ui| {
                        for &(name, cn) in EVENT_TYPES {
                            let label = format!("{} [{}]", name, cn);
                            ui.selectable_value(
                                &mut self.selected_filter,
                                name.to_string(),
                                label,
                            );
                        }
                    });

                ui.separator();
                ui.checkbox(&mut self.auto_scroll, "Auto-scroll");

                ui.separator();
                if ui.button("Clear").clicked() {
                    if let Ok(mut entries) = self.entries.lock() {
                        entries.clear();
                    }
                }

                ui.separator();
                if let Ok(entries) = self.entries.lock() {
                    ui.label(
                        egui::RichText::new(format!("{} events", entries.len()))
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
                .filter(|e| {
                    self.selected_filter == "All"
                        || e.event_type == self.selected_filter
                })
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

                                // 事件类型
                                let color = event_color(&entry.event_type);
                                ui.label(
                                    egui::RichText::new(
                                        format!("{:>22}", entry.event_type),
                                    )
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
        let start = if lines.len() > 50 { lines.len() - 50 } else { 0 };

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
        Some(f) => entry
            .event_type
            .to_lowercase()
            .contains(&f.to_lowercase()),
    }
}

/// 启动 GUI 窗口
pub fn run_gui(filter: Option<&str>) {
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
        "Claude Hook Monitor",
        options,
        Box::new(move |cc| {
            // 加载中文字体
            let mut fonts = egui::FontDefinitions::default();
            if let Ok(font_data) = std::fs::read("/System/Library/Fonts/Supplemental/Arial Unicode.ttf") {
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
