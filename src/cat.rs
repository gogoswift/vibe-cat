use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::{egui, epaint};
use rand::Rng;

use crate::logger;
#[cfg(target_os = "macos")]
use crate::tray;

/// 嵌入精灵图
pub(crate) const CAT_SPRITE_BYTES: &[u8] = include_bytes!("assets/cat.png");
pub(crate) const CAT2_SPRITE_BYTES: &[u8] = include_bytes!("assets/cat2.png");

/// 精灵图网格参数
const CELL_SIZE: u32 = 32;

/// 显示缩放倍数（像素风放大）
const SCALE: f32 = 3.0;

/// 迷你猫缩放
const MINI_SCALE: f32 = 1.5;

/// 动画定义
struct AnimationDef {
    name: &'static str,
    row: u32,
    frame_count: u32,
    frame_duration: Duration,
    /// 移动速度（像素/秒），0 表示静止动画
    move_speed: f32,
}

const ANIMATIONS: &[AnimationDef] = &[
    AnimationDef { name: "sit_1",   row: 0, frame_count: 4, frame_duration: Duration::from_millis(300), move_speed: 0.0 },
    AnimationDef { name: "sit_2",   row: 1, frame_count: 4, frame_duration: Duration::from_millis(300), move_speed: 0.0 },
    AnimationDef { name: "sit_3",   row: 2, frame_count: 4, frame_duration: Duration::from_millis(300), move_speed: 0.0 },
    AnimationDef { name: "sit_4",   row: 3, frame_count: 4, frame_duration: Duration::from_millis(300), move_speed: 0.0 },
    AnimationDef { name: "walk",    row: 4, frame_count: 8, frame_duration: Duration::from_millis(100), move_speed: 60.0 },
    AnimationDef { name: "run",     row: 5, frame_count: 8, frame_duration: Duration::from_millis(100), move_speed: 180.0 },
    AnimationDef { name: "sleep",   row: 6, frame_count: 4, frame_duration: Duration::from_millis(500), move_speed: 0.0 },
    AnimationDef { name: "play",    row: 7, frame_count: 6, frame_duration: Duration::from_millis(200), move_speed: 0.0 },
    AnimationDef { name: "pounce",  row: 8, frame_count: 7, frame_duration: Duration::from_millis(150), move_speed: 40.0 },
    AnimationDef { name: "stretch", row: 9, frame_count: 8, frame_duration: Duration::from_millis(200), move_speed: 0.0 },
];

// 动画索引常量
const ANIM_SLEEP: usize = 6;
const ANIM_POUNCE: usize = 8;
const ANIM_STRETCH: usize = 9;

/// 确认框/通知气泡展开时的窗口宽度
const EXPANDED_W: f32 = 90.0;

/// Claude 状态
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ClaudeState {
    Active,
    Idle,
    Offline,
}

/// 状态对应的动画池
fn state_animations(state: ClaudeState) -> &'static [usize] {
    match state {
        ClaudeState::Active => &[4, 5],           // walk, run
        ClaudeState::Idle => &[0, 1, 2, 3, 7],    // sit_1-4, play
        ClaudeState::Offline => &[ANIM_SLEEP],     // sleep
    }
}

/// 事件类型 -> Claude 状态
fn event_type_to_state(event_type: &str) -> ClaudeState {
    match event_type {
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse"
        | "PostToolUseFailure" | "SubagentStart" | "SubagentStop" | "PreCompact"
        | "InstructionsLoaded" | "WorktreeCreate" | "WorktreeRemove" | "ConfigChange"
        | "conversation_starts" | "api_request" | "tool_decision" | "tool_result" | "sse_event" => {
            ClaudeState::Active
        }
        "Stop" | "PermissionRequest" | "TaskCompleted" | "TeammateIdle" | "Notification" => {
            ClaudeState::Idle
        }
        "SessionEnd" => ClaudeState::Offline,
        _ => ClaudeState::Idle,
    }
}

/// 运行时单帧
struct RuntimeFrame {
    texture: egui::TextureHandle,
    width: f32,
    height: f32,
    bottom_offset: f32,
}

/// 运行时动画组
struct RuntimeAnimation {
    frames: Vec<RuntimeFrame>,
    frame_duration: Duration,
    move_speed: f32,
}

/// 动画状态
struct AnimationState {
    current_anim: usize,
    current_frame: usize,
    last_frame_time: Instant,
    loop_count: u32,
    max_loops: u32,
}

/// 待审批标记
struct PendingApproval;

// ============================================================
// CatEntity: 统一的猫实体（大猫/迷你猫共用）
// ============================================================

struct CatEntity {
    id: String,
    #[allow(dead_code)]
    is_mini: bool,
    #[allow(dead_code)]
    scale: f32,
    animations: Vec<RuntimeAnimation>,
    state: AnimationState,
    /// 窗口内水平偏移（不是屏幕坐标）
    x_offset: f32,
    move_direction: f32,
    last_move_time: Instant,
    claude_state: ClaudeState,
    transition_anim: Option<usize>,
    pending_state: Option<ClaudeState>,
    pending_approval: Option<PendingApproval>,
    bubble_show_time: Option<Instant>,
    notification_text: Option<String>,
    notification_expire: Instant,
    max_width: f32,
    max_height: f32,
    min_bottom_offset: f32,
    /// mini 猫收到 SubagentStop 后标记为 returning，跑向主猫后消失
    returning: bool,
    /// mini 猫创建时间，用于超时兜底
    spawn_time: Instant,
}

impl CatEntity {
    /// 创建主猫：加载全部 10 个动画，SCALE=3.0
    fn new_main(ctx: &egui::Context, sheet: &image::RgbaImage) -> Self {
        let mut animations = Vec::new();
        let mut max_w: f32 = 0.0;
        let mut max_h: f32 = 0.0;
        let mut min_bottom_offset: f32 = f32::MAX;

        for anim_def in ANIMATIONS {
            let mut frames = Vec::new();
            for col in 0..anim_def.frame_count {
                let name = format!("cat_{}_{}", anim_def.name, col);
                let (frame, w, h) = extract_cropped_frame(sheet, anim_def.row, col, ctx, &name, SCALE);
                max_w = max_w.max(w);
                max_h = max_h.max(h + frame.bottom_offset);
                min_bottom_offset = min_bottom_offset.min(frame.bottom_offset);
                frames.push(frame);
            }
            animations.push(RuntimeAnimation {
                frames,
                frame_duration: anim_def.frame_duration,
                move_speed: anim_def.move_speed,
            });
        }
        let min_bottom_offset = if min_bottom_offset == f32::MAX { 0.0 } else { min_bottom_offset };

        let now = Instant::now();
        CatEntity {
            id: "main".to_string(),
            is_mini: false,
            scale: SCALE,
            animations,
            state: AnimationState {
                current_anim: ANIM_SLEEP,
                current_frame: 0,
                last_frame_time: now,
                loop_count: 0,
                max_loops: 3,
            },
            x_offset: 0.0,
            move_direction: 1.0,
            last_move_time: now,
            claude_state: ClaudeState::Offline,
            transition_anim: None,
            pending_state: None,
            pending_approval: None,
            bubble_show_time: None,
            notification_text: None,
            notification_expire: now,
            max_width: max_w,
            max_height: max_h,
            min_bottom_offset,
            returning: false,
            spawn_time: now,
        }
    }

    /// 创建迷你猫：只加载 walk(4) 和 run(5)，MINI_SCALE=1.5
    fn new_mini(ctx: &egui::Context, sheet: &image::RgbaImage, agent_id: &str, start_x: f32) -> Self {
        let mini_anims: &[(usize, f32)] = &[
            (4, 60.0),   // walk
            (5, 180.0),  // run
        ];

        let mut animations = Vec::new();
        let mut max_w: f32 = 0.0;
        let mut max_h: f32 = 0.0;
        let mut min_bottom_offset: f32 = f32::MAX;

        for &(anim_idx, move_speed) in mini_anims {
            let anim_def = &ANIMATIONS[anim_idx];
            let mut frames = Vec::new();
            for col in 0..anim_def.frame_count {
                let name = format!("mini_{}_{}_{}", agent_id, anim_def.name, col);
                let (frame, w, h) =
                    extract_cropped_frame(sheet, anim_def.row, col, ctx, &name, MINI_SCALE);
                max_w = max_w.max(w);
                max_h = max_h.max(h + frame.bottom_offset);
                min_bottom_offset = min_bottom_offset.min(frame.bottom_offset);
                frames.push(frame);
            }
            animations.push(RuntimeAnimation {
                frames,
                frame_duration: anim_def.frame_duration,
                move_speed,
            });
        }
        let min_bottom_offset = if min_bottom_offset == f32::MAX { 0.0 } else { min_bottom_offset };

        let now = Instant::now();
        let mut rng = rand::thread_rng();
        let start_anim = rng.gen_range(0..animations.len());
        let direction = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };

        CatEntity {
            id: agent_id.to_string(),
            is_mini: true,
            scale: MINI_SCALE,
            animations,
            state: AnimationState {
                current_anim: start_anim,
                current_frame: 0,
                last_frame_time: now,
                loop_count: 0,
                max_loops: rng.gen_range(3..8),
            },
            x_offset: start_x,
            move_direction: direction,
            last_move_time: now,
            claude_state: ClaudeState::Active,
            transition_anim: None,
            pending_state: None,
            pending_approval: None,
            bubble_show_time: None,
            notification_text: None,
            notification_expire: now,
            max_width: max_w,
            max_height: max_h,
            min_bottom_offset,
            returning: false,
            spawn_time: now,
        }
    }

    /// 切换到状态对应的随机动画
    fn switch_to_state_animation(&mut self, state: ClaudeState) {
        let pool = state_animations(state);
        let mut rng = rand::thread_rng();
        let mut next = pool[rng.gen_range(0..pool.len())];
        if pool.len() > 1 {
            while next == self.state.current_anim {
                next = pool[rng.gen_range(0..pool.len())];
            }
        }
        self.state.current_anim = next;
        self.state.current_frame = 0;
        self.state.loop_count = 0;
        self.state.max_loops = rng.gen_range(2..5);

        if self.animations[next].move_speed > 0.0 {
            self.move_direction = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
        }
    }

    /// 切换到指定动画（用于过渡动画）
    fn switch_to_animation(&mut self, anim_idx: usize) {
        self.state.current_anim = anim_idx;
        self.state.current_frame = 0;
        self.state.loop_count = 0;
        self.state.max_loops = 1;

        if self.animations[anim_idx].move_speed > 0.0 {
            let mut rng = rand::thread_rng();
            self.move_direction = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
        }
    }
}

/// 从精灵图中提取并裁剪一个帧
fn extract_cropped_frame(
    sheet: &image::RgbaImage,
    row: u32,
    col: u32,
    ctx: &egui::Context,
    name: &str,
    scale: f32,
) -> (RuntimeFrame, f32, f32) {
    let cell_x = col * CELL_SIZE;
    let cell_y = row * CELL_SIZE;

    let mut min_x = CELL_SIZE;
    let mut min_y = CELL_SIZE;
    let mut max_x = 0u32;
    let mut max_y = 0u32;

    for ly in 0..CELL_SIZE {
        for lx in 0..CELL_SIZE {
            let gx = cell_x + lx;
            let gy = cell_y + ly;
            if gx < sheet.width() && gy < sheet.height() {
                let px = sheet.get_pixel(gx, gy);
                if px[3] > 0 {
                    min_x = min_x.min(lx);
                    min_y = min_y.min(ly);
                    max_x = max_x.max(lx);
                    max_y = max_y.max(ly);
                }
            }
        }
    }

    if max_x < min_x || max_y < min_y {
        let color_image = egui::ColorImage::new([1, 1], egui::Color32::TRANSPARENT);
        let texture = ctx.load_texture(name, color_image, egui::TextureOptions::NEAREST);
        return (
            RuntimeFrame {
                texture,
                width: scale,
                height: scale,
                bottom_offset: 0.0,
            },
            scale,
            scale,
        );
    }

    let crop_w = (max_x - min_x + 1) as usize;
    let crop_h = (max_y - min_y + 1) as usize;

    let mut pixels = Vec::with_capacity(crop_w * crop_h);
    for ly in min_y..=max_y {
        for lx in min_x..=max_x {
            let px = sheet.get_pixel(cell_x + lx, cell_y + ly);
            pixels.push(egui::Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]));
        }
    }

    let color_image = egui::ColorImage {
        size: [crop_w, crop_h],
        pixels,
    };

    let texture = ctx.load_texture(name, color_image, egui::TextureOptions::NEAREST);

    let w = crop_w as f32 * scale;
    let h = crop_h as f32 * scale;
    let bottom_off = (CELL_SIZE - max_y - 1) as f32 * scale;

    (
        RuntimeFrame {
            texture,
            width: w,
            height: h,
            bottom_offset: bottom_off,
        },
        w,
        h,
    )
}

// ============================================================
// UnifiedCatApp: 统一窗口，所有猫在同一个 Dock 宽度窗口内
// ============================================================

/// 后台线程传回的 Dock 边界数据
#[derive(Clone)]
struct DockBoundsResult {
    dock_left: f32,
    dock_right: f32,
    base_y: f32,
}

struct UnifiedCatApp {
    sprite_sheet: image::RgbaImage,
    cx_sprite_sheet: image::RgbaImage,
    main_cat: CatEntity,
    mini_cats: Vec<CatEntity>,
    position_phase: u32,
    window_width: f32,
    window_height: f32,
    dock_left: f32,
    dock_right: f32,
    base_y: f32,
    last_poll_time: Instant,
    last_dock_refresh: Instant,
    last_event_time: Option<String>,
    /// agent_id → 创建时间
    known_subagents: HashSet<String>,
    debug_subagents_active: bool,
    /// 后台 Dock 刷新结果
    dock_result: Arc<Mutex<Option<DockBoundsResult>>>,
    dock_refreshing: Arc<Mutex<bool>>,
    /// 应用启动时间 (用于 zzz 等持续动画的时间基准)
    app_start: Instant,
    /// 拖拽偏移量（拖拽中 / 回弹中生效）
    drag_offset: egui::Vec2,
    /// 是否正在拖拽大猫
    is_dragging: bool,
    /// 下落动画: (起始时间, 起始 y 偏移)
    snap_back_start: Option<(Instant, f32)>,
    /// 上一帧大猫绘制矩形（用于鼠标命中检测）
    last_cat_rect: egui::Rect,
    /// 托盘动画状态
    #[cfg(target_os = "macos")]
    tray_anim_state: tray::TrayAnimState,
    #[cfg(target_os = "macos")]
    tray_last_frame_time: Instant,
    #[cfg(target_os = "macos")]
    tray_status_item: Option<objc2::rc::Retained<objc2_app_kit::NSStatusItem>>,
    #[cfg(target_os = "macos")]
    tray_nsimages: Vec<Vec<objc2::rc::Retained<objc2_app_kit::NSImage>>>,
}

impl UnifiedCatApp {
    fn new(ctx: &egui::Context, dock_left: f32, dock_right: f32, visible_bottom: f32) -> Self {
        #[cfg(target_os = "macos")]
        hide_dock_icon();
        Self::setup_chinese_font(ctx);

        let sheet = image::load_from_memory(CAT_SPRITE_BYTES)
            .expect("Failed to decode cat sprite sheet")
            .to_rgba8();

        let cx_sheet = image::load_from_memory(CAT2_SPRITE_BYTES)
            .expect("Failed to decode cat2 sprite sheet")
            .to_rgba8();

        let main_cat = CatEntity::new_main(ctx, &sheet);

        let window_width = dock_right - dock_left;
        let window_height = main_cat.max_height + 22.0;

        // 初始化托盘图标
        #[cfg(target_os = "macos")]
        let (tray_status_item, tray_nsimages) = {
            let sprite_dyn = image::load_from_memory(CAT_SPRITE_BYTES)
                .expect("Failed to decode sprite for tray");
            let png_frames = tray::precompute_tray_frames(&sprite_dyn);
            let nsimages = tray::create_tray_nsimages(&png_frames);
            let item = tray::create_status_item(&nsimages);
            (Some(item), nsimages)
        };

        let now = Instant::now();
        UnifiedCatApp {
            sprite_sheet: sheet,
            cx_sprite_sheet: cx_sheet,
            main_cat,
            mini_cats: Vec::new(),
            position_phase: 0,
            window_width,
            window_height,
            dock_left,
            dock_right,
            base_y: visible_bottom,
            last_poll_time: now,
            last_dock_refresh: now,
            last_event_time: None,
            known_subagents: HashSet::new(),
            debug_subagents_active: false,
            dock_result: Arc::new(Mutex::new(None)),
            dock_refreshing: Arc::new(Mutex::new(false)),
            app_start: now,
            drag_offset: egui::Vec2::ZERO,
            is_dragging: false,
            snap_back_start: None,
            last_cat_rect: egui::Rect::NOTHING,
            #[cfg(target_os = "macos")]
            tray_anim_state: tray::TrayAnimState::new(),
            #[cfg(target_os = "macos")]
            tray_last_frame_time: now,
            #[cfg(target_os = "macos")]
            tray_status_item,
            #[cfg(target_os = "macos")]
            tray_nsimages,
        }
    }

    fn setup_chinese_font(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        let font_paths = [
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ];
        for path in &font_paths {
            if let Ok(font_data) = std::fs::read(path) {
                fonts.font_data.insert(
                    "chinese".to_owned(),
                    egui::FontData::from_owned(font_data).into(),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push("chinese".to_owned());
                break;
            }
        }
        ctx.set_fonts(fonts);
    }

    /// 轮询 Claude 事件状态（适配自原 CatApp::poll_claude_state）
    /// 返回需要新建的 mini cat agent_id 列表
    fn poll_claude_state(&mut self) -> Vec<String> {
        let entries = logger::read_recent_entries(200).unwrap_or_default();
        let mut new_agents = Vec::new();

        // 检测 subagent 生命周期事件
        for entry in &entries {
            let is_new = match &self.last_event_time {
                Some(t) => entry.timestamp > *t,
                None => false,
            };
            if !is_new {
                continue;
            }

            if entry.event_type == "SubagentStart" {
                if let Some(aid) = entry.raw.get("agent_id")
                    .or_else(|| entry.raw.get("call_id"))
                    .and_then(|v| v.as_str())
                {
                    let qualified_id = if entry.source == "cx" {
                        format!("cx:{}", aid)
                    } else {
                        aid.to_string()
                    };
                    if !qualified_id.is_empty() && !self.known_subagents.contains(&qualified_id) {
                        self.known_subagents.insert(qualified_id.clone());
                        new_agents.push(qualified_id);
                    }
                }
            } else if entry.event_type == "SubagentStop" {
                if let Some(aid) = entry.raw.get("agent_id")
                    .or_else(|| entry.raw.get("call_id"))
                    .and_then(|v| v.as_str())
                {
                    let qualified_id = if entry.source == "cx" {
                        format!("cx:{}", aid)
                    } else {
                        aid.to_string()
                    };
                    self.known_subagents.remove(&qualified_id);
                    for mc in &mut self.mini_cats {
                        if mc.id == qualified_id {
                            mc.returning = true;
                        }
                    }
                }
            }
        }

        // 扫描新事件，检测快速 Active->Idle 序列
        let mut had_active_in_new_events = false;
        for entry in &entries {
            let is_new = match &self.last_event_time {
                Some(t) => entry.timestamp > *t,
                None => true,
            };
            let is_subagent = entry.raw.get("agent_type").and_then(|v| v.as_str()).is_some();
            if is_new && !is_subagent && event_type_to_state(&entry.event_type) == ClaudeState::Active {
                had_active_in_new_events = true;
            }
        }

        // 找最后一条主代理事件来决定主猫状态
        let last_non_subagent = entries.iter().rev().find(|e| {
            e.raw.get("agent_type").and_then(|v| v.as_str()).is_none()
        });
        let new_state = if let Some(entry) = last_non_subagent {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&entry.timestamp) {
                let age = chrono::Local::now().signed_duration_since(ts);
                if age > chrono::Duration::minutes(5) {
                    ClaudeState::Offline
                } else {
                    // 检测 elicitation 通知
                    if entry.event_type == "Notification" {
                        if let Some(raw) = entry.raw.get("notification_type") {
                            if raw.as_str() == Some("elicitation_dialog") {
                                self.main_cat.notification_text =
                                    Some("等你回答".to_string());
                                self.main_cat.notification_expire =
                                    Instant::now() + Duration::from_secs(8);
                            }
                        }
                    }
                    event_type_to_state(&entry.event_type)
                }
            } else {
                ClaudeState::Offline
            }
        } else {
            ClaudeState::Offline
        };

        // 避免重复处理
        let current_event_time = entries.last().map(|e| e.timestamp.clone());
        if current_event_time == self.last_event_time && new_state == self.main_cat.claude_state {
            return new_agents;
        }
        self.last_event_time = current_event_time;

        // 状态变化处理
        if new_state != self.main_cat.claude_state {
            if self.main_cat.transition_anim.is_some() {
                self.main_cat.pending_state = Some(new_state);
                return new_agents;
            }

            let old = self.main_cat.claude_state;
            self.main_cat.claude_state = new_state;

            if old == ClaudeState::Offline
                && (new_state == ClaudeState::Idle || new_state == ClaudeState::Active)
            {
                self.main_cat.transition_anim = Some(ANIM_STRETCH);
                self.main_cat.pending_state = Some(new_state);
                self.main_cat.switch_to_animation(ANIM_STRETCH);
            } else if old == ClaudeState::Active
                && (new_state == ClaudeState::Idle || new_state == ClaudeState::Offline)
            {
                self.main_cat.transition_anim = Some(ANIM_POUNCE);
                self.main_cat.pending_state = Some(new_state);
                self.main_cat.switch_to_animation(ANIM_POUNCE);
            } else if had_active_in_new_events && new_state != ClaudeState::Active {
                self.main_cat.transition_anim = Some(ANIM_POUNCE);
                self.main_cat.pending_state = Some(new_state);
                self.main_cat.switch_to_animation(ANIM_POUNCE);
            } else {
                self.main_cat.switch_to_state_animation(new_state);
            }
        } else if had_active_in_new_events
            && new_state != ClaudeState::Active
            && self.main_cat.transition_anim.is_none()
        {
            self.main_cat.transition_anim = Some(ANIM_POUNCE);
            self.main_cat.pending_state = Some(new_state);
            self.main_cat.switch_to_animation(ANIM_POUNCE);
        }

        new_agents
    }

    /// 空格键：切换模拟 3 个 subagent start/stop 事件
    fn toggle_debug_subagents(&mut self) {
        use std::io::Write;
        let log_path = logger::log_file_path();
        let event_type = if self.debug_subagents_active {
            "SubagentStop"
        } else {
            "SubagentStart"
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            for i in 1..=3 {
                let ts = chrono::Local::now().to_rfc3339();
                let line = format!(
                    "{{\"timestamp\":\"{}\",\"event_type\":\"{}\",\"session_id\":\"debug\",\"summary\":\"debug subagent {}\",\"raw\":{{\"agent_id\":\"debug-agent-{}\"}}}}\n",
                    ts, event_type, i, i
                );
                let _ = file.write_all(line.as_bytes());
            }
        }
        self.debug_subagents_active = !self.debug_subagents_active;
    }

    /// 检查 pending approval 文件，气泡显示 5 秒后自动消失
    fn poll_pending_approval(&mut self) {
        let pending_path = dirs::home_dir()
            .unwrap()
            .join(".claude-hook-monitor")
            .join("pending-approval.json");

        if pending_path.exists() {
            if let Some(start) = self.main_cat.bubble_show_time {
                if start.elapsed() > Duration::from_secs(5) {
                    let _ = std::fs::remove_file(&pending_path);
                    self.main_cat.pending_approval = None;
                    self.main_cat.bubble_show_time = None;
                }
            } else {
                self.main_cat.pending_approval = Some(PendingApproval);
                self.main_cat.bubble_show_time = Some(Instant::now());
            }
        } else {
            self.main_cat.pending_approval = None;
            self.main_cat.bubble_show_time = None;
        }
    }

    /// 启动后台线程刷新 Dock 边界（非阻塞）
    fn start_dock_refresh_bg(&self) {
        let mut refreshing = self.dock_refreshing.lock().unwrap();
        if *refreshing {
            return; // 已有后台任务在跑
        }
        *refreshing = true;

        let result_arc = Arc::clone(&self.dock_result);
        let refreshing_arc = Arc::clone(&self.dock_refreshing);

        std::thread::spawn(move || {
            #[cfg(target_os = "macos")]
            {
                if let Some((screen_w, visible_bottom, _)) = get_macos_screen_info() {
                    let (dl, dr) = get_dock_bounds(screen_w, visible_bottom);
                    let mut result = result_arc.lock().unwrap();
                    *result = Some(DockBoundsResult {
                        dock_left: dl,
                        dock_right: dr,
                        base_y: visible_bottom,
                    });
                }
            }
            let mut refreshing = refreshing_arc.lock().unwrap();
            *refreshing = false;
        });
    }

    /// 从后台结果应用 Dock 边界，返回宽度是否变化
    fn apply_dock_result(&mut self) -> bool {
        let result = {
            let mut lock = self.dock_result.lock().unwrap();
            lock.take()
        };
        if let Some(bounds) = result {
            let old_width = self.window_width;
            self.dock_left = bounds.dock_left;
            self.dock_right = bounds.dock_right;
            self.window_width = bounds.dock_right - bounds.dock_left;
            self.base_y = bounds.base_y;

            // 钳位所有猫（内联以避免借用冲突）
            let ww = self.window_width;
            {
                let max_x = (ww - self.main_cat.max_width).max(0.0);
                if self.main_cat.x_offset < 0.0 { self.main_cat.x_offset = 0.0; }
                if self.main_cat.x_offset > max_x { self.main_cat.x_offset = max_x; }
            }
            for mc in &mut self.mini_cats {
                let max_x = (ww - mc.max_width).max(0.0);
                if mc.x_offset < 0.0 { mc.x_offset = 0.0; }
                if mc.x_offset > max_x { mc.x_offset = max_x; }
            }

            return (self.window_width - old_width).abs() > 1.0;
        }
        false
    }

}

impl eframe::App for UnifiedCatApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(16));

        let now = Instant::now();

        // 空格键：模拟 subagent
        let space_pressed = ctx.input(|i| i.key_pressed(egui::Key::Space));
        if space_pressed {
            self.toggle_debug_subagents();
        }

        // ---- 分阶段初始化窗口位置 ----
        if self.position_phase < 10 {
            self.position_phase += 1;
            if self.position_phase == 1 {
                // Phase 1: 设置窗口尺寸为 Dock 宽度
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                    egui::vec2(self.window_width, self.window_height),
                ));
            } else if self.position_phase == 5 {
                // Phase 5: 定位到 Dock 上方
                #[cfg(target_os = "macos")]
                {
                    let title_bar_h = ctx.input(|i| {
                        if let (Some(outer), Some(inner)) =
                            (i.viewport().outer_rect, i.viewport().inner_rect)
                        {
                            outer.height() - inner.height()
                        } else {
                            0.0
                        }
                    });
                    let x = self.dock_left;
                    let y = self.base_y - self.window_height - title_bar_h
                        + self.main_cat.min_bottom_offset;
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                        egui::pos2(x, y),
                    ));
                    // 大猫居中
                    self.main_cat.x_offset =
                        (self.window_width - self.main_cat.max_width) / 2.0;
                    self.main_cat.last_move_time = now;
                }
            } else if self.position_phase == 6 {
                // Phase 6: 初始化窗口外观
                setup_window_appearance();
            }
            return; // 定位阶段不绘制
        }

        // ---- 每秒轮询事件 + 每 500ms 检查 pending approval ----
        if now.duration_since(self.last_poll_time) > Duration::from_millis(500) {
            if now.duration_since(self.last_poll_time) > Duration::from_secs(1) {
                // 轮询事件，收集需要新建的 mini cat ids
                let new_agent_ids = self.poll_claude_state();
                // 在事件循环之后创建 CatEntity，避免借用冲突
                for aid in new_agent_ids {
                    let start_x = self.main_cat.x_offset;
                    let sheet = if aid.starts_with("cx:") {
                        &self.cx_sprite_sheet
                    } else {
                        &self.sprite_sheet
                    };
                    let mini = CatEntity::new_mini(ctx, sheet, &aid, start_x);
                    self.mini_cats.push(mini);
                }
                self.last_poll_time = now;
            }
            self.poll_pending_approval();
        }

        // ---- 后台 Dock 刷新（每 5 秒触发，不阻塞 UI） ----
        if now.duration_since(self.last_dock_refresh) > Duration::from_secs(5) {
            self.last_dock_refresh = now;
            self.start_dock_refresh_bg();
        }
        // 检查后台结果并应用
        let bounds_changed = self.apply_dock_result();
        if bounds_changed {
            self.window_height = self.main_cat.max_height + 22.0;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                egui::vec2(self.window_width, self.window_height),
            ));
            #[cfg(target_os = "macos")]
            {
                let title_bar_h = ctx.input(|i| {
                    if let (Some(outer), Some(inner)) =
                        (i.viewport().outer_rect, i.viewport().inner_rect)
                    {
                        outer.height() - inner.height()
                    } else {
                        0.0
                    }
                });
                let y = self.base_y - self.window_height - title_bar_h
                    + self.main_cat.min_bottom_offset;
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                    egui::pos2(self.dock_left, y),
                ));
            }
        }

        // ---- 托盘图标动画 (每 250ms) ----
        #[cfg(target_os = "macos")]
        {
            if now.duration_since(self.tray_last_frame_time) >= Duration::from_millis(250) {
                self.tray_last_frame_time = now;
                tray::sync_tray_state(&mut self.tray_anim_state, self.main_cat.claude_state);
                tray::advance_tray_animation(&mut self.tray_anim_state);
                if let Some(ref item) = self.tray_status_item {
                    tray::update_tray_icon(item, &self.tray_anim_state, &self.tray_nsimages);
                }
            }
        }

        // ---- 猫隐藏时跳过渲染（托盘动画仍继续） ----
        #[cfg(target_os = "macos")]
        {
            if !tray::CAT_VISIBLE.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
        }

        // ---- 推进主猫动画帧（拖拽/下落中暂停） ----
        let cat_suspended = self.is_dragging || self.snap_back_start.is_some();
        if !cat_suspended {
            let cat = &mut self.main_cat;
            // 提取 frame_duration 值（Copy），避免延长不可变借用
            let frame_dur = cat.animations[cat.state.current_anim].frame_duration;
            let frame_count = cat.animations[cat.state.current_anim].frames.len();
            if now.duration_since(cat.state.last_frame_time) >= frame_dur {
                cat.state.current_frame += 1;
                if cat.state.current_frame >= frame_count {
                    cat.state.current_frame = 0;
                    cat.state.loop_count += 1;
                    if cat.state.loop_count >= cat.state.max_loops {
                        if cat.transition_anim.is_some() {
                            cat.transition_anim = None;
                            let target = cat.pending_state.take().unwrap_or(cat.claude_state);
                            if target != cat.claude_state {
                                let old = cat.claude_state;
                                cat.claude_state = target;
                                if old == ClaudeState::Active
                                    && (target == ClaudeState::Idle || target == ClaudeState::Offline)
                                {
                                    cat.transition_anim = Some(ANIM_POUNCE);
                                    cat.pending_state = Some(target);
                                    cat.switch_to_animation(ANIM_POUNCE);
                                } else if old == ClaudeState::Offline
                                    && (target == ClaudeState::Idle || target == ClaudeState::Active)
                                {
                                    cat.transition_anim = Some(ANIM_STRETCH);
                                    cat.pending_state = Some(target);
                                    cat.switch_to_animation(ANIM_STRETCH);
                                } else {
                                    cat.switch_to_state_animation(target);
                                }
                            } else {
                                cat.switch_to_state_animation(target);
                            }
                        } else {
                            cat.switch_to_state_animation(cat.claude_state);
                        }
                    }
                }
                // 使用 += 保留溢出时间；若落后超过 1 帧则跳到当前（防追帧闪烁）
                cat.state.last_frame_time += frame_dur;
                if now.duration_since(cat.state.last_frame_time) >= frame_dur {
                    cat.state.last_frame_time = now;
                }
            }
        }

        // ---- 推进迷你猫动画帧 ----
        for mc in &mut self.mini_cats {
            let anim_dur = mc.animations[mc.state.current_anim].frame_duration;
            let anim_len = mc.animations[mc.state.current_anim].frames.len();
            if now.duration_since(mc.state.last_frame_time) >= anim_dur {
                mc.state.current_frame += 1;
                if mc.state.current_frame >= anim_len {
                    mc.state.current_frame = 0;
                    mc.state.loop_count += 1;
                    if mc.state.loop_count >= mc.state.max_loops {
                        let mut rng = rand::thread_rng();
                        let next = (mc.state.current_anim + 1) % mc.animations.len();
                        mc.state.current_anim = next;
                        mc.state.current_frame = 0;
                        mc.state.loop_count = 0;
                        mc.state.max_loops = rng.gen_range(3..8);
                        if rng.gen_bool(0.3) {
                            mc.move_direction = -mc.move_direction;
                        }
                    }
                }
                // 使用 += 保留溢出时间；若落后超过 1 帧则跳到当前
                mc.state.last_frame_time += anim_dur;
                if now.duration_since(mc.state.last_frame_time) >= anim_dur {
                    mc.state.last_frame_time = now;
                }
            }
        }

        // ---- 更新主猫位置（拖拽/下落中暂停） ----
        if !cat_suspended {
            let cat = &mut self.main_cat;
            let move_speed = cat.animations[cat.state.current_anim].move_speed;
            if move_speed > 0.0 {
                let dt = now.duration_since(cat.last_move_time).as_secs_f32();
                cat.last_move_time = now;
                cat.x_offset += cat.move_direction * move_speed * dt;

                let max_x = (self.window_width - cat.max_width).max(0.0);
                if cat.x_offset < 0.0 {
                    cat.x_offset = 0.0;
                    cat.move_direction = 1.0;
                }
                if cat.x_offset > max_x {
                    cat.x_offset = max_x;
                    cat.move_direction = -1.0;
                }
            } else {
                cat.last_move_time = now;
            }
        } else {
            // 拖拽/下落中：重置 last_move_time 防止恢复后瞬移
            self.main_cat.last_move_time = now;
        }

        // ---- 迷你猫超时兜底：10 分钟未收到 SubagentStop 则自动返回 ----
        for mc in &mut self.mini_cats {
            if !mc.returning && now.duration_since(mc.spawn_time) > Duration::from_secs(600) {
                mc.returning = true;
            }
        }

        // ---- 更新迷你猫位置 ----
        let ww = self.window_width;
        let main_center_x = self.main_cat.x_offset + self.main_cat.max_width / 2.0;
        for mc in &mut self.mini_cats {
            let move_speed = mc.animations[mc.state.current_anim].move_speed;
            if move_speed > 0.0 {
                let dt = now.duration_since(mc.last_move_time).as_secs_f32();
                mc.last_move_time = now;

                if mc.returning {
                    // returning：朝主猫中心跑，用 run 速度
                    let mc_center = mc.x_offset + mc.max_width / 2.0;
                    let diff = main_center_x - mc_center;
                    mc.move_direction = if diff > 0.0 { 1.0 } else { -1.0 };
                    // 用最快的动画（run = index 1）
                    if mc.state.current_anim != 1 && mc.animations.len() > 1 {
                        mc.state.current_anim = 1;
                        mc.state.current_frame = 0;
                    }
                    mc.x_offset += mc.move_direction * mc.animations[1].move_speed * dt;
                } else {
                    mc.x_offset += mc.move_direction * move_speed * dt;
                }

                let max_x = (ww - mc.max_width).max(0.0);
                if mc.x_offset < 0.0 {
                    mc.x_offset = 0.0;
                    if !mc.returning { mc.move_direction = 1.0; }
                }
                if mc.x_offset > max_x {
                    mc.x_offset = max_x;
                    if !mc.returning { mc.move_direction = -1.0; }
                }
            } else {
                mc.last_move_time = now;
            }
        }
        // 到达主猫身边的 returning 猫：删除
        self.mini_cats.retain(|mc| {
            if !mc.returning { return true; }
            let mc_center = mc.x_offset + mc.max_width / 2.0;
            (mc_center - main_center_x).abs() > 5.0
        });

        // ---- 拖拽下落动画（垂直落回底部，x 保持不变） ----
        if let Some((start_time, start_y)) = self.snap_back_start {
            let elapsed = now.duration_since(start_time).as_secs_f32();
            let duration = 0.25; // 250ms 下落
            if elapsed >= duration {
                // 落地：把水平拖拽偏移合并进猫的 x_offset，然后清零
                self.main_cat.x_offset = (self.main_cat.x_offset + self.drag_offset.x)
                    .clamp(0.0, (self.window_width - self.main_cat.max_width).max(0.0));
                self.drag_offset = egui::Vec2::ZERO;
                self.snap_back_start = None;
                // 重置动画时间基准，防止恢复后追帧
                self.main_cat.state.last_frame_time = now;
            } else {
                let t = elapsed / duration;
                // ease-in quad: 模拟重力加速下落
                let ease = t * t;
                self.drag_offset.y = start_y * (1.0 - ease);
            }
        }

        // ---- 动态鼠标穿透（仅当鼠标在猫精灵上时关闭穿透） ----
        update_mouse_passthrough(self.last_cat_rect, self.is_dragging);

        // ---- 绘制 ----
        let panel_frame = egui::Frame::NONE
            .fill(egui::Color32::TRANSPARENT);

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                let available = ui.available_rect_before_wrap();

                // 先画迷你猫（在下层）
                for mc in &self.mini_cats {
                    let f = &mc.animations[mc.state.current_anim].frames[mc.state.current_frame];
                    let x = available.min.x + mc.x_offset + (mc.max_width - f.width) / 2.0;
                    let y = available.max.y - f.height - f.bottom_offset;

                    let rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(f.width, f.height),
                    );

                    let move_speed = mc.animations[mc.state.current_anim].move_speed;
                    let uv = if mc.move_direction < 0.0 && move_speed > 0.0 {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                    };

                    ui.painter().image(f.texture.id(), rect, uv, egui::Color32::WHITE);
                }

                // 再画大猫（在上层）+ 拖拽交互
                {
                    let cat = &self.main_cat;
                    let f = &cat.animations[cat.state.current_anim].frames[cat.state.current_frame];
                    let base_x = available.min.x + cat.x_offset + (cat.max_width - f.width) / 2.0;
                    let base_y = available.max.y - f.height - f.bottom_offset;

                    // 应用拖拽偏移，并约束在窗口区域内
                    let x = (base_x + self.drag_offset.x)
                        .clamp(available.min.x, available.max.x - f.width);
                    let y = (base_y + self.drag_offset.y)
                        .clamp(available.min.y, available.max.y - f.height);

                    let rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(f.width, f.height),
                    );

                    // 保存猫矩形供下帧鼠标穿透检测使用
                    self.last_cat_rect = rect;

                    let move_speed = cat.animations[cat.state.current_anim].move_speed;
                    let uv = if cat.move_direction < 0.0 && move_speed > 0.0 {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                    };

                    ui.painter().image(f.texture.id(), rect, uv, egui::Color32::WHITE);

                    // ---- 拖拽交互 ----
                    let drag_response = ui.interact(
                        rect,
                        egui::Id::new("main_cat_drag"),
                        egui::Sense::drag(),
                    );
                    if drag_response.dragged() {
                        self.is_dragging = true;
                        self.snap_back_start = None; // 打断正在回弹的动画
                        self.drag_offset += drag_response.drag_delta();
                        // 约束拖拽范围在窗口内
                        let min_ox = available.min.x - base_x;
                        let max_ox = available.max.x - f.width - base_x;
                        let min_oy = available.min.y - base_y;
                        let max_oy = 0.0_f32; // 不能拖到底部以下
                        self.drag_offset.x = self.drag_offset.x.clamp(min_ox, max_ox);
                        self.drag_offset.y = self.drag_offset.y.clamp(min_oy, max_oy);
                    }
                    if drag_response.drag_stopped() {
                        self.is_dragging = false;
                        if self.drag_offset.y.abs() > 0.5 {
                            // 开始垂直下落动画（x 保持不变）
                            self.snap_back_start = Some((now, self.drag_offset.y));
                        } else {
                            self.drag_offset.y = 0.0;
                        }
                    }

                    // 睡觉时头顶飘 zzz
                    if cat.state.current_anim == ANIM_SLEEP {
                        // 用应用启动以来的时间驱动，Z 的浮动不受帧重置影响
                        let t_global = self.app_start.elapsed().as_secs_f32();
                        // 3 个 Z，间隔 0.8 秒，每个生命周期 2.4 秒
                        let cycle = 2.4_f32;
                        let stagger = 0.8_f32;
                        let head_x = rect.center().x + rect.width() * 0.25;
                        let head_y = rect.min.y + 4.0;

                        for i in 0..3u32 {
                            let phase = (t_global + i as f32 * stagger) % cycle;
                            let progress = phase / cycle; // 0.0 → 1.0

                            // 向上漂浮 30pt, 向右偏移 8pt
                            let float_y = head_y - progress * 30.0;
                            let drift_x = head_x + i as f32 * 4.0 + progress * 8.0;

                            // 透明度: 淡入(0~10%) → 保持(10~70%) → 淡出(70~100%)
                            let alpha = if progress < 0.1 {
                                progress / 0.1
                            } else if progress < 0.7 {
                                1.0
                            } else {
                                1.0 - (progress - 0.7) / 0.3
                            };

                            // 字号随飘升变小: 14 → 10
                            let font_size = 14.0 - progress * 4.0;
                            let a = (alpha * 220.0) as u8;

                            ui.painter().text(
                                egui::pos2(drift_x, float_y),
                                egui::Align2::LEFT_BOTTOM,
                                "z",
                                egui::FontId::proportional(font_size),
                                egui::Color32::from_rgba_unmultiplied(200, 200, 255, a),
                            );
                        }
                    }

                    // 大猫上方的气泡 -- PermissionRequest
                    if cat.pending_approval.is_some() {
                        let bubble_w = EXPANDED_W;
                        let bubble_x = available.min.x + cat.x_offset
                            + (cat.max_width - bubble_w) / 2.0 + self.drag_offset.x;
                        let bubble_y = y - 22.0;
                        let bubble_rect = egui::Rect::from_min_size(
                            egui::pos2(bubble_x.max(available.min.x), bubble_y.max(available.min.y)),
                            egui::vec2(bubble_w, 18.0),
                        );
                        ui.painter().rect_filled(
                            bubble_rect, 4.0,
                            egui::Color32::from_rgba_unmultiplied(245, 243, 240, 230),
                        );
                        ui.painter().rect_stroke(
                            bubble_rect, 4.0,
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 40)),
                            epaint::StrokeKind::Outside,
                        );
                        ui.painter().text(
                            bubble_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "需要人工介入",
                            egui::FontId::proportional(10.0),
                            egui::Color32::from_rgba_unmultiplied(40, 40, 40, 255),
                        );
                    }

                    // Elicitation 通知气泡
                    if let Some(ref text) = cat.notification_text {
                        if now < cat.notification_expire {
                            let bubble_w = EXPANDED_W;
                            let bubble_x = available.min.x + cat.x_offset
                                + (cat.max_width - bubble_w) / 2.0 + self.drag_offset.x;
                            let bubble_y = y - 22.0;
                            let bubble_rect = egui::Rect::from_min_size(
                                egui::pos2(bubble_x.max(available.min.x), bubble_y.max(available.min.y)),
                                egui::vec2(bubble_w, 18.0),
                            );
                            ui.painter().rect_filled(
                                bubble_rect, 4.0,
                                egui::Color32::from_rgba_unmultiplied(245, 243, 240, 230),
                            );
                            ui.painter().rect_stroke(
                                bubble_rect, 4.0,
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 40)),
                                epaint::StrokeKind::Outside,
                            );
                            ui.painter().text(
                                bubble_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                text,
                                egui::FontId::proportional(9.0),
                                egui::Color32::from_rgba_unmultiplied(40, 40, 40, 255),
                            );
                        }
                    }
                }

                // 清理已过期通知
                if let Some(ref _text) = self.main_cat.notification_text {
                    if now >= self.main_cat.notification_expire {
                        self.main_cat.notification_text = None;
                    }
                }
            });
    }
}

// ============================================================
// macOS 平台函数
// ============================================================

/// macOS: 获取 Dock 水平边界
///
/// 策略: 优先通过 Accessibility API 获取 Dock 真实边界 (精确),
///       无权限时 fallback 到 defaults + lsappinfo 估算
#[cfg(target_os = "macos")]
fn get_dock_bounds(screen_w: f32, _dock_y: f32) -> (f32, f32) {
    // 优先尝试 Accessibility API
    if let Some((left, right)) = get_dock_bounds_ax(screen_w) {
        return (left, right);
    }

    // Fallback: 估算
    get_dock_bounds_estimate(screen_w)
}

/// 通过 Accessibility API 直接读取 Dock 的 AXList 元素边界 (精确方法)
///
/// 权限状态由 `check_ax_permission` 在启动时一次性检查并缓存
#[cfg(target_os = "macos")]
fn get_dock_bounds_ax(screen_w: f32) -> Option<(f32, f32)> {
    use std::ffi::c_void;
    use std::process::Command;
    use std::ptr;
    use std::sync::atomic::{AtomicU8, Ordering};

    // 权限缓存: 0=未检查, 1=有权限, 2=无权限
    static AX_PERMISSION: AtomicU8 = AtomicU8::new(0);

    // Accessibility API FFI 声明
    type AXUIElementRef = *mut c_void;
    type CFTypeRef = *mut c_void;
    type CFStringRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type AXError = i32;
    const SUCCESS: AXError = 0;
    const AX_VALUE_CGPOINT: u32 = 1;
    const AX_VALUE_CGSIZE: u32 = 2;

    extern "C" {
        fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXValueGetValue(value: CFTypeRef, value_type: u32, out: *mut c_void) -> bool;
        fn CFRelease(cf: *const c_void);
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, idx: isize) -> CFTypeRef;
    }

    // 权限检查策略:
    //   perm==0 (未检查): 看磁盘标记 → 没弹过则弹一次并写标记, 弹过则静默检查
    //   perm==1 (有权限): 直接走 AX 路径
    //   perm==2 (无权限): 直接跳过
    let perm = AX_PERMISSION.load(Ordering::Relaxed);
    if perm == 2 {
        return None;
    }

    // 标记文件: 记录是否已经弹过授权对话框 (永久只弹一次)
    let marker_path = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("claude-hook-monitor")
        .join(".ax_prompted");

    let is_trusted = unsafe {
        use objc2_foundation::{NSDictionary, NSNumber, NSString};
        let key = NSString::from_str("AXTrustedCheckOptionPrompt");

        // 只有从未弹过对话框时才 prompt=true
        let already_prompted = marker_path.exists();
        let prompt = perm == 0 && !already_prompted;

        let val = NSNumber::new_bool(prompt);
        let dict = NSDictionary::from_id_slice(&[&*key], &[val]);
        let dict_ptr = &*dict as *const _ as CFDictionaryRef;
        let trusted = AXIsProcessTrustedWithOptions(dict_ptr);

        // 弹过一次后写入标记文件
        if prompt {
            if let Some(parent) = marker_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&marker_path, "1");
        }

        trusted
    };

    if is_trusted {
        AX_PERMISSION.store(1, Ordering::Relaxed);
    } else {
        AX_PERMISSION.store(2, Ordering::Relaxed);
        return None;
    }

    // 获取 Dock 进程 PID (通过 pgrep, 快速且可靠)
    let dock_pid: i32 = Command::new("pgrep")
        .args(&["-x", "Dock"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())?;

    // 创建 CFString (NSString 与 CFStringRef 是 toll-free bridged)
    // 绑定到变量以确保生命周期, 函数结束时自动释放
    use objc2_foundation::NSString;
    let children_key = NSString::from_str("AXChildren");
    let role_key = NSString::from_str("AXRole");
    let position_key = NSString::from_str("AXPosition");
    let size_key = NSString::from_str("AXSize");

    let children_attr = &*children_key as *const NSString as CFStringRef;
    let role_attr = &*role_key as *const NSString as CFStringRef;
    let position_attr = &*position_key as *const NSString as CFStringRef;
    let size_attr = &*size_key as *const NSString as CFStringRef;

    unsafe {
        let dock_el = AXUIElementCreateApplication(dock_pid);
        if dock_el.is_null() {
            return None;
        }

        // 获取 Dock 的子元素
        let mut children: CFTypeRef = ptr::null_mut();
        if AXUIElementCopyAttributeValue(dock_el, children_attr, &mut children) != SUCCESS {
            CFRelease(dock_el);
            return None;
        }

        let count = CFArrayGetCount(children);
        let mut result: Option<(f32, f32)> = None;

        for i in 0..count {
            let child = CFArrayGetValueAtIndex(children, i);

            // 检查角色是否为 AXList (Dock 的图标列表)
            let mut role: CFTypeRef = ptr::null_mut();
            if AXUIElementCopyAttributeValue(child, role_attr, &mut role) != SUCCESS {
                continue;
            }
            let role_ns: &objc2_foundation::NSString =
                &*(role as *const objc2_foundation::NSString);
            let is_list = role_ns.to_string() == "AXList";
            CFRelease(role);

            if !is_list {
                continue;
            }

            // 获取 AXList 的 position 和 size → 即 Dock 的真实边界
            let mut pos_val: CFTypeRef = ptr::null_mut();
            let mut size_val: CFTypeRef = ptr::null_mut();

            if AXUIElementCopyAttributeValue(child, position_attr, &mut pos_val) != SUCCESS {
                continue;
            }
            if AXUIElementCopyAttributeValue(child, size_attr, &mut size_val) != SUCCESS {
                CFRelease(pos_val);
                continue;
            }

            let mut pos: [f64; 2] = [0.0; 2]; // CGPoint {x, y}
            let mut size: [f64; 2] = [0.0; 2]; // CGSize {width, height}

            AXValueGetValue(pos_val, AX_VALUE_CGPOINT, pos.as_mut_ptr() as *mut c_void);
            AXValueGetValue(size_val, AX_VALUE_CGSIZE, size.as_mut_ptr() as *mut c_void);

            CFRelease(pos_val);
            CFRelease(size_val);

            let dock_left = pos[0] as f32;
            let dock_right = (pos[0] + size[0]) as f32;
            let dock_height = size[1] as f32;

            // 用 AXList 高度推算圆角: squircle 圆角 ≈ 高度 × 0.27
            let corner_r = (dock_height * 0.27).max(10.0);
            let left = dock_left + corner_r;
            let right = dock_right - corner_r;

            if right > left && left >= 0.0 && right <= screen_w {
                result = Some((left, right));
            }
            break;
        }

        CFRelease(children);
        CFRelease(dock_el);
        result
    }
}

/// Fallback: 通过 Dock 偏好设置 + lsappinfo 估算 Dock 水平边界
///
/// Dock 布局 (从左到右):
///   Finder | 固定应用 | 非固定运行应用 | 分隔线 | [最近应用 | 分隔线] | 持久化其他项 | 废纸篓
#[cfg(target_os = "macos")]
fn get_dock_bounds_estimate(screen_w: f32) -> (f32, f32) {
    use std::collections::HashSet;
    use std::process::Command;

    let run_cmd = |cmd: &str, args: &[&str]| -> String {
        Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    // ---- 1. 图标大小 ----
    let tilesize: f32 = run_cmd("defaults", &["read", "com.apple.dock", "tilesize"])
        .parse()
        .unwrap_or(48.0);

    // ---- 2. 固定应用 (persistent-apps): 提取 bundle ID 集合 + 数量 ----
    let pinned_apps_str = run_cmd("defaults", &["read", "com.apple.dock", "persistent-apps"]);
    let pinned_app_count = pinned_apps_str.matches("tile-data").count();
    let pinned_bundle_ids: HashSet<&str> = pinned_apps_str
        .lines()
        .filter_map(|line| {
            if line.contains("bundle-identifier") {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 {
                    Some(parts[3])
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    // ---- 3. 持久化其他项 (persistent-others): 下载、应用程序等 ----
    let pinned_others: usize =
        run_cmd("defaults", &["read", "com.apple.dock", "persistent-others"])
            .matches("tile-data")
            .count();

    // ---- 4. 通过 lsappinfo 获取前台运行应用 (~70ms, 比 osascript 376ms 快 5 倍) ----
    let lsappinfo_out = run_cmd("lsappinfo", &["list"]);
    let mut running_non_pinned: usize = 0;
    let mut current_bundle_id: &str = "";

    for line in lsappinfo_out.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("bundleID=") {
            let bid = rest.trim_matches('"');
            if bid != "[ NULL ]" {
                current_bundle_id = bid;
            } else {
                current_bundle_id = "";
            }
        }
        if trimmed.contains("type=\"Foreground\"") && !current_bundle_id.is_empty() {
            if current_bundle_id != "com.apple.finder"
                && !pinned_bundle_ids.contains(current_bundle_id)
            {
                running_non_pinned += 1;
            }
            current_bundle_id = "";
        }
    }

    // ---- 5. 最近使用的应用 (show-recents) ----
    let show_recents: bool = run_cmd("defaults", &["read", "com.apple.dock", "show-recents"])
        .parse::<i32>()
        .unwrap_or(0)
        != 0;
    let recent_count: usize = if show_recents { 3 } else { 0 };

    // ---- 6. 计算总项目数 ----
    let apps_count = 1 + pinned_app_count + running_non_pinned; // Finder + pinned + non-pinned
    let others_count = pinned_others + 1; // others + Trash
    let total_items = apps_count + recent_count + others_count;
    let separators: usize = if show_recents { 2 } else { 1 };

    let item_slot = tilesize + 4.0;
    let dock_width = total_items as f32 * item_slot + separators as f32 * 12.0 + 16.0;

    let dock_left = (screen_w - dock_width) / 2.0;
    let dock_right = dock_left + dock_width;

    // Dock 背景高度 ≈ tilesize + 20pt, squircle 圆角 ≈ 背景高度 × 0.27
    let dock_bg_height = tilesize + 20.0;
    let corner_r = (dock_bg_height * 0.27).max(10.0);
    let left = (dock_left + corner_r).max(0.0);
    let right = (dock_right - corner_r).min(screen_w);

    (left, right)
}

/// macOS: 通过 NSScreen 获取屏幕可用区域
#[cfg(target_os = "macos")]
fn get_macos_screen_info() -> Option<(f32, f32, f32)> {
    use objc2_app_kit::NSScreen;
    use objc2_foundation::MainThreadMarker;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let screen = NSScreen::mainScreen(mtm)?;
    let full = screen.frame();
    let visible = screen.visibleFrame();

    let screen_w = full.size.width as f32;
    let screen_h = full.size.height as f32;

    let menu_bar_h = screen_h - visible.origin.y as f32 - visible.size.height as f32;
    let visible_bottom = screen_h - visible.origin.y as f32;

    Some((screen_w, visible_bottom, menu_bar_h))
}

/// macOS: 隐藏 Dock 图标
#[cfg(target_os = "macos")]
pub fn hide_dock_icon() {
    use objc2_app_kit::NSApplication;
    use objc2_app_kit::NSApplicationActivationPolicy;
    use objc2_foundation::MainThreadMarker;
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
}

/// macOS: 初始化窗口外观（去阴影 + 默认鼠标穿透）
#[cfg(target_os = "macos")]
fn setup_window_appearance() {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    if let Some(window) = unsafe { app.mainWindow() } {
        unsafe {
            // 默认鼠标穿透（后续会根据鼠标位置动态切换）
            let _: () = objc2::msg_send![&*window, setIgnoresMouseEvents: true];
            // 去掉窗口阴影
            let _: () = objc2::msg_send![&*window, setHasShadow: false];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn setup_window_appearance() {}

/// macOS: 根据鼠标位置动态切换鼠标穿透
///
/// - 鼠标在猫精灵上时：关闭穿透，允许拖拽交互
/// - 鼠标在透明区域时：开启穿透，点击可到达下方窗口/Dock
/// - 正在拖拽时：始终关闭穿透
#[cfg(target_os = "macos")]
fn update_mouse_passthrough(cat_rect: egui::Rect, is_dragging: bool) {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{CGPoint, CGRect, MainThreadMarker};
    use objc2::runtime::{AnyClass, AnyObject};

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);

    unsafe {
        // 获取第一个窗口
        let windows: *mut AnyObject = objc2::msg_send![&*app, windows];
        let count: usize = objc2::msg_send![windows, count];
        if count == 0 {
            return;
        }
        let window: *mut AnyObject = objc2::msg_send![windows, objectAtIndex: 0_usize];
        if window.is_null() {
            return;
        }

        // 拖拽中：始终允许鼠标事件
        if is_dragging {
            let _: () = objc2::msg_send![window, setIgnoresMouseEvents: false];
            return;
        }

        // 获取鼠标屏幕坐标（macOS 坐标系：原点左下，y 向上）
        let ns_event_cls = AnyClass::get("NSEvent").unwrap();
        let mouse: CGPoint = objc2::msg_send![ns_event_cls, mouseLocation];

        // 获取窗口 frame（屏幕坐标）
        let frame: CGRect = objc2::msg_send![window, frame];

        // 转换为窗口内坐标（egui 坐标系：原点左上，y 向下）
        let local_x = (mouse.x - frame.origin.x) as f32;
        let local_y = (frame.size.height - (mouse.y - frame.origin.y)) as f32;

        // 检查是否在猫精灵范围内（加 10px 外扩以提高命中手感）
        let padded = cat_rect.expand(10.0);
        let over_cat = padded.contains(egui::pos2(local_x, local_y));

        let _: () = objc2::msg_send![window, setIgnoresMouseEvents: !over_cat];
    }
}

#[cfg(not(target_os = "macos"))]
fn update_mouse_passthrough(_cat_rect: egui::Rect, _is_dragging: bool) {}

// ============================================================
// 入口函数
// ============================================================

pub fn run_cat() {
    #[cfg(target_os = "macos")]
    let (initial_pos, dock_left, dock_right, visible_bottom) = {
        if let Some((screen_w, vis_bottom, _menu_bar_h)) = get_macos_screen_info() {
            let (dl, dr) = get_dock_bounds(screen_w, vis_bottom);
            let _window_width = dr - dl;
            let window_height = CELL_SIZE as f32 * SCALE + 22.0;
            let x = dl;
            let y = vis_bottom - window_height;
            ([x, y], dl, dr, vis_bottom)
        } else {
            ([200.0, 600.0], 200.0, 1200.0, 800.0)
        }
    };

    #[cfg(not(target_os = "macos"))]
    let (initial_pos, dock_left, dock_right, visible_bottom) =
        ([200.0, 600.0], 200.0, 1200.0, 800.0);

    let window_width = dock_right - dock_left;
    let window_height = CELL_SIZE as f32 * SCALE + 22.0;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([window_width, window_height])
            .with_position(initial_pos)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top(),
        ..Default::default()
    };

    let dl = dock_left;
    let dr = dock_right;
    let vb = visible_bottom;

    eframe::run_native(
        "Desktop Cat",
        options,
        Box::new(move |cc| Ok(Box::new(UnifiedCatApp::new(&cc.egui_ctx, dl, dr, vb)))),
    )
    .expect("Failed to start cat window");
}
