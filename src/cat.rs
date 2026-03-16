//! 桌面像素猫渲染与状态同步模块。
//!
//! 职责与边界：
//! - 负责加载精灵、推进动画、处理拖拽交互，并将事件日志映射为猫咪状态。
//! - 负责依据 Dock 区域更新窗口位置与宽度，让猫停靠在 Dock 上方。
//! - 负责在 macOS 下采样多屏幕几何快照，并据此刷新 Dock 边界。
//! - 不负责日志采集、OTel HTTP 接收与安装流程（这些由其他模块处理）。
//!
//! 关键副作用：
//! - 调用窗口相关 API 改变位置、尺寸、阴影与鼠标穿透行为。
//! - 周期性启动后台线程刷新 Dock 边界估算结果。
//! - 在 GUI 启动时拉起 OTel 接收线程（见 `run_cat`）。
//!
//! 关键依赖与约束：
//! - 依赖 `eframe/egui`、`objc2` 与 `image`。
//! - AppKit 的 `NSScreen` 查询必须在主线程执行；后台线程只消费已采样数据。
//! - Dock 定位与托盘逻辑仅在 macOS 生效。

use std::collections::{HashMap, HashSet};
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
    AnimationDef {
        name: "sit_1",
        row: 0,
        frame_count: 4,
        frame_duration: Duration::from_millis(300),
        move_speed: 0.0,
    },
    AnimationDef {
        name: "sit_2",
        row: 1,
        frame_count: 4,
        frame_duration: Duration::from_millis(300),
        move_speed: 0.0,
    },
    AnimationDef {
        name: "sit_3",
        row: 2,
        frame_count: 4,
        frame_duration: Duration::from_millis(300),
        move_speed: 0.0,
    },
    AnimationDef {
        name: "sit_4",
        row: 3,
        frame_count: 4,
        frame_duration: Duration::from_millis(300),
        move_speed: 0.0,
    },
    AnimationDef {
        name: "walk",
        row: 4,
        frame_count: 8,
        frame_duration: Duration::from_millis(100),
        move_speed: 60.0,
    },
    AnimationDef {
        name: "run",
        row: 5,
        frame_count: 8,
        frame_duration: Duration::from_millis(100),
        move_speed: 180.0,
    },
    AnimationDef {
        name: "sleep",
        row: 6,
        frame_count: 4,
        frame_duration: Duration::from_millis(500),
        move_speed: 0.0,
    },
    AnimationDef {
        name: "play",
        row: 7,
        frame_count: 6,
        frame_duration: Duration::from_millis(200),
        move_speed: 0.0,
    },
    AnimationDef {
        name: "pounce",
        row: 8,
        frame_count: 7,
        frame_duration: Duration::from_millis(150),
        move_speed: 40.0,
    },
    AnimationDef {
        name: "stretch",
        row: 9,
        frame_count: 8,
        frame_duration: Duration::from_millis(200),
        move_speed: 0.0,
    },
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
        ClaudeState::Active => &[4, 5],        // walk, run
        ClaudeState::Idle => &[0, 1, 2, 3, 7], // sit_1-4, play
        ClaudeState::Offline => &[ANIM_SLEEP], // sleep
    }
}

/// 事件类型 -> Claude 状态
fn event_type_to_state(event_type: &str) -> ClaudeState {
    match event_type {
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        | "SubagentStart" | "SubagentStop" | "PreCompact" | "WorktreeCreate" | "WorktreeRemove"
        | "api_request" | "tool_decision" | "tool_result" | "sse_event" => ClaudeState::Active,
        "SessionStart" | "Stop" | "PermissionRequest" | "TaskCompleted" | "TeammateIdle"
        | "Notification" => ClaudeState::Idle,
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
    pending_permission_count: usize,
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
                let (frame, w, h) =
                    extract_cropped_frame(sheet, anim_def.row, col, ctx, &name, SCALE);
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
        let min_bottom_offset = if min_bottom_offset == f32::MAX {
            0.0
        } else {
            min_bottom_offset
        };

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
            pending_permission_count: 0,
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
    fn new_mini(
        ctx: &egui::Context,
        sheet: &image::RgbaImage,
        agent_id: &str,
        start_x: f32,
    ) -> Self {
        let mini_anims: &[(usize, f32)] = &[
            (4, 60.0),  // walk
            (5, 180.0), // run
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
        let min_bottom_offset = if min_bottom_offset == f32::MAX {
            0.0
        } else {
            min_bottom_offset
        };

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
            pending_permission_count: 0,
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
            pixels.push(egui::Color32::from_rgba_unmultiplied(
                px[0], px[1], px[2], px[3],
            ));
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

/// 后台线程传回的完整布局结果。
///
/// 语义与边界：
/// - 线程间只传递 `AppliedLayout`，避免 UI 线程自行拼装局部字段。
/// - 结果始终对应同一轮 Dock 采样快照。
///
/// 关键副作用：
/// - 无。
#[derive(Clone, Debug)]
struct DockBoundsResult {
    layout: AppliedLayout,
}

/// UI 线程已应用的窗口布局快照。
///
/// 语义与边界：
/// - `window_origin` 记录窗口定位基准：`x` 为窗口左上角，`y` 为基线换算值。
/// - `walk_bounds` 描述猫可活动区域，宽度同时作为窗口宽度来源。
/// - `dock_mode` 与 `anchor_screen_id` 共同标识布局策略与锚定屏幕。
/// - `dock_autohide` 记录 Dock 是否自动隐藏，供轮询节流策略使用。
///
/// 关键副作用：
/// - 无。
#[derive(Clone, Debug)]
struct AppliedLayout {
    anchor_screen_id: String,
    window_origin: egui::Pos2,
    walk_bounds: crate::cat_layout::Rect,
    dock_mode: crate::cat_layout::DockPlacementMode,
    dock_autohide: bool,
    window_width: f32,
}

impl AppliedLayout {
    /// 构建跨平台兜底布局快照。
    ///
    /// 入参：
    /// - `left`/`right`：窗口水平边界（point）。
    /// - `base_y`：窗口 Y 轴换算所需基线值（point）。
    ///
    /// 返回：
    /// - 使用 Floor 模式的兜底布局快照。
    ///
    /// 错误处理与失败场景：
    /// - 不返回错误；当 `right < left` 时宽度会被钳为 `0`。
    ///
    /// 关键副作用：
    /// - 无。
    fn fallback(left: f32, right: f32, base_y: f32) -> Self {
        let width = (right - left).max(0.0);
        Self {
            anchor_screen_id: "fallback".to_string(),
            window_origin: egui::pos2(left, base_y),
            walk_bounds: crate::cat_layout::Rect::new(left, base_y, width, 0.0),
            dock_mode: crate::cat_layout::DockPlacementMode::Floor,
            dock_autohide: false,
            window_width: width,
        }
    }
}

/// 结合 `dock_frame` 与 `visible_frame` 计算 Bottom Dock 的窗口基线。
///
/// 语义与边界：
/// - 运行时主窗口使用 winit/macOS 的“相对主屏顶部、Y 轴向下”坐标系。
/// - AX 路径下的 `dock_frame.y` 表示 Dock 顶边的 AppKit 全局 Y；估算路径下
///   的 `dock_frame.y` 则可能表示 Dock 底边，因此需要结合 `visible_frame`
///   判定实际含义。
/// - 若 `visible_frame` 已经保留出比 `dock_frame` 更高的安全区，优先使用更保守
///   的那一条基线，避免猫被 Dock 挡住。
///
/// 入参：
/// - `screens`：当前轮次屏幕快照，用于取得主显示器高度。
/// - `anchor_screen`：当前 Dock 锚点屏幕。
/// - `dock_frame`：Bottom 模式 Dock 几何（AppKit 全局坐标）。
///
/// 返回：
/// - 对齐 winit/macOS 坐标系的 Dock 顶边基线值（point）。
///
/// 错误处理与失败场景：
/// - 不返回错误；主显示器高度缺失时退回锚点屏幕高度。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn bottom_dock_base_y_from_frame(
    screens: &[MacScreenSnapshot],
    anchor_screen: &MacScreenSnapshot,
    dock_frame: &crate::cat_layout::Rect,
) -> f32 {
    const DOCK_FRAME_VISIBLE_BOTTOM_EPSILON: f32 = 1.0;

    let main_height = main_display_height(screens).unwrap_or(anchor_screen.frame.height);
    let dock_top_in_appkit = if ((dock_frame.y + dock_frame.height) - anchor_screen.visible_frame.y)
        .abs()
        <= DOCK_FRAME_VISIBLE_BOTTOM_EPSILON
    {
        dock_frame.y + dock_frame.height
    } else {
        dock_frame.y
    };

    main_height - dock_top_in_appkit
}

/// 解析运行时 `AppliedLayout` 应使用的窗口 Y 基线。
///
/// 语义与边界：
/// - `Floor` 模式继续沿用 `visible_frame` 的底边语义。
/// - `Bottom` 模式会同时参考 `visible_frame` 与 `dock_frame`：
///   - `visible_frame` 正常时维持现有更保守的保留高度；
///   - `visible_frame` 因自动隐藏或副屏切换丢失底边内缩时，回退到实时 Dock 几何。
///
/// 入参：
/// - `screens`：当前轮次屏幕快照。
/// - `anchor_screen`：Dock 锚点屏幕。
/// - `dock_sample`：当前 Dock 采样结果。
///
/// 返回：
/// - 运行时窗口定位使用的 Y 基线（point）。
///
/// 错误处理与失败场景：
/// - 不返回错误；Dock 几何缺失时退回 `visible_frame` 基线。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn applied_layout_base_y(
    screens: &[MacScreenSnapshot],
    anchor_screen: &MacScreenSnapshot,
    dock_sample: &DockPlacementSample,
) -> f32 {
    let visible_base_y = legacy_visible_bottom(screens, anchor_screen);

    if dock_sample.dock_snapshot.mode != crate::cat_layout::DockPlacementMode::Bottom {
        return visible_base_y;
    }

    dock_sample
        .dock_snapshot
        .dock_frame
        .as_ref()
        .map(|dock_frame| {
            visible_base_y.min(bottom_dock_base_y_from_frame(
                screens,
                anchor_screen,
                dock_frame,
            ))
        })
        .unwrap_or(visible_base_y)
}

/// 使用纯布局计算结果构建运行时 `AppliedLayout`。
///
/// 语义与边界：
/// - 运行时窗口 Y 坐标会转换到 winit/macOS 使用的“相对主屏顶部”的坐标系（见
///   `legacy_visible_bottom` / `applied_layout_base_y`），避免上下堆叠多屏时把窗口
///   算到错误屏幕，也避免副屏/自动隐藏场景下被 Dock 挡住。
/// - 水平方向（窗口宽度/活动范围）完全复用 `cat_layout::compute_cat_window_layout` 的输出，
///   避免测试与运行时逻辑分叉。
/// - `dock_autohide` 仅做透传，不参与本函数内的几何推断。
///
/// 入参：
/// - `screens`：当前轮次采样得到的 macOS 屏幕快照。
/// - `fallback_screen`：兜底屏幕（通常为主屏），用于计算基线时的回退。
/// - `dock_sample`：Dock 采样结果，包含 `DockSnapshot` 与（可选）活动范围覆盖值。
///
/// 返回：
/// - `Some(AppliedLayout)`：输入完整且可计算。
/// - `None`：找不到锚点屏幕，或 Bottom 模式缺失 Dock 矩形。
///
/// 错误处理与失败场景：
/// - 使用 `Option` 表达可恢复输入缺失，不抛异常。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn compute_applied_layout_for_dock_sample(
    screens: &[MacScreenSnapshot],
    fallback_screen: &MacScreenSnapshot,
    dock_sample: &DockPlacementSample,
) -> Option<AppliedLayout> {
    let screen_snapshots: Vec<crate::cat_layout::ScreenSnapshot> = screens
        .iter()
        .map(|screen| {
            crate::cat_layout::ScreenSnapshot::new(
                screen.id.clone(),
                screen.frame.clone(),
                screen.visible_frame.clone(),
                screen.scale_factor,
            )
        })
        .collect();

    // 运行时当前仍使用固定的窗口底部留白语义（22pt），与历史实现保持一致。
    let layout = crate::cat_layout::compute_cat_window_layout(
        &screen_snapshots,
        &dock_sample.dock_snapshot,
        CELL_SIZE as f32 * SCALE,
        22.0,
    )?;

    let walk_bounds = layout.walk_bounds;
    let window_width = walk_bounds.width;

    let anchor_screen = screen_by_id(screens, &layout.anchor_screen_id).unwrap_or(fallback_screen);
    let base_y = applied_layout_base_y(screens, anchor_screen, dock_sample);

    Some(AppliedLayout {
        anchor_screen_id: layout.anchor_screen_id,
        window_origin: egui::pos2(walk_bounds.x, base_y),
        walk_bounds,
        dock_mode: layout.mode,
        dock_autohide: dock_sample.dock_snapshot.autohide,
        window_width,
    })
}

/// 根据 Dock 自动隐藏状态决定后台刷新节流间隔。
///
/// 语义与边界：
/// - 自动隐藏开启时使用更快轮询，降低 Dock 弹出/收起带来的定位滞后。
/// - 自动隐藏关闭时使用慢轮询，避免不必要的后台采样。
///
/// 入参：
/// - `autohide`：`true` 表示 Dock 自动隐藏已开启。
///
/// 返回：
/// - `Duration::from_millis(250)`（`autohide=true`）或 `Duration::from_secs(5)`（`autohide=false`）。
///
/// 错误处理与失败场景：
/// - 不返回错误。
///
/// 关键副作用：
/// - 无。
fn refresh_interval(autohide: bool) -> Duration {
    if autohide {
        Duration::from_millis(250)
    } else {
        Duration::from_secs(5)
    }
}

/// 解析 Dock 刷新调度使用的自动隐藏状态。
///
/// 语义与边界：
/// - 优先使用最新实时探测值，避免调度依赖滞后的已应用布局状态。
/// - 无实时探测值时，回退到已应用布局中的 `dock_autohide`。
/// - 二者都缺失时按 `false` 处理，保持历史兜底语义。
///
/// 入参：
/// - `applied_layout`：已应用布局快照，可为空。
/// - `live_probe_autohide`：实时探测到的 Dock 自动隐藏状态，可为空。
///
/// 返回：
/// - 当前调度应使用的自动隐藏状态。
///
/// 错误处理与失败场景：
/// - 不返回错误。
///
/// 关键副作用：
/// - 无。
fn resolve_refresh_autohide(
    applied_layout: Option<&AppliedLayout>,
    live_probe_autohide: Option<bool>,
) -> bool {
    live_probe_autohide
        .or_else(|| applied_layout.map(|layout| layout.dock_autohide))
        .unwrap_or(false)
}

/// 计算本轮更新应使用的 Dock 刷新间隔。
///
/// 语义与边界：
/// - 间隔由 `resolve_refresh_autohide` 决定的自动隐藏状态驱动。
/// - 实时探测值存在时可覆盖已应用布局，避免自动隐藏切换后的调度迟滞。
///
/// 入参：
/// - `applied_layout`：已应用布局快照，可为空。
/// - `live_probe_autohide`：实时探测到的自动隐藏状态，可为空。
///
/// 返回：
/// - 当前 tick 应使用的 Dock 刷新间隔。
///
/// 错误处理与失败场景：
/// - 不返回错误。
///
/// 关键副作用：
/// - 无。
fn dock_refresh_interval_for_tick(
    applied_layout: Option<&AppliedLayout>,
    live_probe_autohide: Option<bool>,
) -> Duration {
    refresh_interval(resolve_refresh_autohide(
        applied_layout,
        live_probe_autohide,
    ))
}

/// 判断是否需要启动高频自动隐藏探测。
///
/// 语义与边界：
/// - 仅在当前已应用布局仍是“非自动隐藏”时返回 `true`。
/// - `None` 视为尚未确认状态，保持探测开启以尽快发现自动隐藏切换。
///
/// 入参：
/// - `applied_layout`：当前已应用布局快照，可为空。
///
/// 返回：
/// - `true`：应启动 250ms 高频自动隐藏探测。
/// - `false`：无需额外高频探测。
///
/// 错误处理与失败场景：
/// - 不返回错误。
///
/// 关键副作用：
/// - 无。
fn should_run_fast_autohide_probe(applied_layout: Option<&AppliedLayout>) -> bool {
    applied_layout
        .map(|layout| !layout.dock_autohide)
        .unwrap_or(true)
}

/// 判断新布局是否需要触发窗口重摆。
///
/// 语义与边界：
/// - 首次应用布局时总是返回 `true`。
/// - 比较锚点屏幕、Dock 模式、窗口原点、活动区域与窗口宽度。
/// - 浮点字段使用阈值比较，避免子像素抖动导致频繁重摆。
///
/// 入参：
/// - `previous`：已应用布局，可为空。
/// - `next`：待应用布局。
///
/// 返回：
/// - `true`：需要重摆窗口。
/// - `false`：布局可视为未变化。
///
/// 错误处理与失败场景：
/// - 不返回错误；浮点比较使用固定阈值。
///
/// 关键副作用：
/// - 无。
fn layout_changed(previous: Option<&AppliedLayout>, next: &AppliedLayout) -> bool {
    const LAYOUT_EPSILON: f32 = 0.5;
    let eq_f32 = |lhs: f32, rhs: f32| (lhs - rhs).abs() <= LAYOUT_EPSILON;

    match previous {
        Some(current) => {
            current.anchor_screen_id != next.anchor_screen_id
                || current.dock_mode != next.dock_mode
                || !eq_f32(current.window_origin.x, next.window_origin.x)
                || !eq_f32(current.window_origin.y, next.window_origin.y)
                || !eq_f32(current.walk_bounds.x, next.walk_bounds.x)
                || !eq_f32(current.walk_bounds.y, next.walk_bounds.y)
                || !eq_f32(current.walk_bounds.width, next.walk_bounds.width)
                || !eq_f32(current.walk_bounds.height, next.walk_bounds.height)
                || current.dock_autohide != next.dock_autohide
                || !eq_f32(current.window_width, next.window_width)
        }
        None => true,
    }
}

/// 基于采样布局计算下一次“已应用布局”状态。
///
/// 语义与边界：
/// - `None` 表示无需推进已应用布局状态。
/// - 该函数仅决定状态推进，不负责窗口命令发送。
///
/// 入参：
/// - `previous`：当前已应用布局快照，可为空。
/// - `sampled`：本轮采样得到的布局快照。
///
/// 返回：
/// - 下一次应保存的已应用布局快照；若无需推进返回 `None`。
///
/// 错误处理与失败场景：
/// - 不返回错误；由调用方保证 `sampled` 有效。
///
/// 关键副作用：
/// - 无。
fn resolve_next_applied_layout(
    previous: Option<&AppliedLayout>,
    sampled: &AppliedLayout,
) -> Option<AppliedLayout> {
    if layout_changed(previous, sampled) {
        Some(sampled.clone())
    } else {
        None
    }
}

/// Dock 水平边界与锚点屏幕。
///
/// 语义与边界：
/// - `left`/`right` 是全局 point 坐标。
/// - `anchor_screen_id` 必须来自同一轮屏幕采样快照。
/// - `dock_frame` 在 AX 路径下保存真实 Dock 矩形；估算路径可为空。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
#[derive(Clone, Debug)]
struct DockHorizontalBounds {
    anchor_screen_id: String,
    left: f32,
    right: f32,
    dock_frame: Option<crate::cat_layout::Rect>,
}

/// Dock 采样统一输出。
///
/// 语义与边界：
/// - `dock_snapshot` 对齐 `cat_layout::DockSnapshot`，用于描述 Bottom/Floor 模式。
/// - `walk_bounds` 描述运行时水平活动区域，单位为全局 point 坐标。
///
/// 入参：
/// - 无。
///
/// 返回：
/// - 无。
#[cfg(target_os = "macos")]
#[derive(Clone, Debug)]
struct DockPlacementSample {
    dock_snapshot: crate::cat_layout::DockSnapshot,
    walk_bounds: crate::cat_layout::Rect,
}

/// 像素猫显示位置模式。
///
/// 语义与边界：
/// - `Auto` 表示跟随当前 Dock 所在显示器。
/// - `Specific(selection_id)` 表示手动绑定到某个运行期显示器标识。
/// - 该模式仅描述“目标显示器选择”，不直接承载 Dock 几何或窗口坐标。
///
/// 关键副作用：
/// - 无；仅作为运行期配置值在模块间传递。
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DisplayLocationMode {
    Auto,
    Specific(String),
}

/// 托盘菜单使用的显示器选项快照。
///
/// 语义与边界：
/// - `selection_id` 是运行期用于匹配显示器选择的稳定标识。
/// - `label` 是当前会话中展示给用户的名称；若名称重复，会追加序号区分。
/// - `is_main` 仅用于 UI 层辅助展示或排序，当前不参与布局公式。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DisplayChoice {
    pub selection_id: String,
    pub label: String,
    pub is_main: bool,
}

/// macOS 屏幕快照（全局坐标）。
///
/// 语义与边界：
/// - `id` 用于在刷新周期内稳定标识屏幕。
/// - `selection_id` 用于跨刷新周期匹配用户手动选择的显示器。
/// - `name` 为当前系统提供的显示器名称，用于菜单展示。
/// - `frame` / `visible_frame` 都是全局 point 坐标。
/// - `scale_factor` 只做透传，当前任务不参与尺寸换算。
/// - `is_main` 记录是否为主屏，用于定位默认锚点。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
#[derive(Clone, Debug)]
struct MacScreenSnapshot {
    id: String,
    selection_id: String,
    name: String,
    frame: crate::cat_layout::Rect,
    visible_frame: crate::cat_layout::Rect,
    scale_factor: f32,
    is_main: bool,
}

struct UnifiedCatApp {
    sprite_sheet: image::RgbaImage,
    cx_sprite_sheet: image::RgbaImage,
    main_cat: CatEntity,
    cx_main_cat: CatEntity,
    mini_cats: Vec<CatEntity>,
    position_phase: u32,
    applied_layout: Option<AppliedLayout>,
    window_width: f32,
    window_height: f32,
    dock_left: f32,
    dock_right: f32,
    base_y: f32,
    last_poll_time: Instant,
    last_dock_refresh: Instant,
    #[cfg(target_os = "macos")]
    last_display_location_revision: u64,
    /// 最近一次实时探测到的 Dock 自动隐藏状态。
    dock_autohide_probe: Option<bool>,
    /// 最近一次执行 Dock 自动隐藏实时探测的时间。
    last_dock_autohide_probe: Instant,
    /// 后台自动隐藏探测结果。
    dock_autohide_probe_result: Arc<Mutex<Option<bool>>>,
    /// 后台自动隐藏探测是否进行中。
    dock_autohide_probe_refreshing: Arc<Mutex<bool>>,
    last_event_time: Option<String>,
    /// agent_id → 创建时间
    known_subagents: HashSet<String>,
    debug_subagents_active: bool,
    /// 后台 Dock 刷新结果
    dock_result: Arc<Mutex<Option<DockBoundsResult>>>,
    dock_refreshing: Arc<Mutex<bool>>,
    /// 应用启动时间 (用于 zzz 等持续动画的时间基准)
    app_start: Instant,
    /// cc 拖拽偏移量（拖拽中 / 回弹中生效）
    drag_offset: egui::Vec2,
    /// 是否正在拖拽 cc 大猫
    is_dragging: bool,
    /// cc 下落动画: (起始时间, 起始 y 偏移)
    snap_back_start: Option<(Instant, f32)>,
    /// 上一帧 cc 大猫绘制矩形（用于鼠标命中检测）
    last_cat_rect: egui::Rect,
    /// cx 拖拽偏移量
    cx_drag_offset: egui::Vec2,
    /// 是否正在拖拽 cx 大猫
    cx_is_dragging: bool,
    /// cx 下落动画
    cx_snap_back_start: Option<(Instant, f32)>,
    /// 上一帧 cx 大猫绘制矩形
    cx_last_cat_rect: egui::Rect,
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
    /// 创建统一猫窗口应用状态。
    ///
    /// 语义与边界：
    /// - 基于 `initial_layout` 初始化窗口宽度、Dock 活动范围和定位基线。
    /// - 仅做内存初始化，不会立即发送窗口重摆命令。
    ///
    /// 入参：
    /// - `ctx`：egui 上下文，用于加载纹理与字体。
    /// - `initial_layout`：启动阶段计算出的完整布局快照。
    ///
    /// 返回：
    /// - 可进入 `update` 循环的 `UnifiedCatApp`。
    ///
    /// 错误处理与失败场景：
    /// - 精灵图解码失败会直接 panic，保持历史启动语义。
    ///
    /// 关键副作用：
    /// - 可能初始化 macOS 托盘图标与 Dock 图标隐藏行为。
    fn new(ctx: &egui::Context, initial_layout: AppliedLayout) -> Self {
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
        let cx_main_cat = CatEntity::new_main(ctx, &cx_sheet);

        let dock_left = initial_layout.window_origin.x;
        let window_width = initial_layout.window_width;
        let dock_right = dock_left + window_width;
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
            cx_main_cat,
            mini_cats: Vec::new(),
            position_phase: 0,
            applied_layout: Some(initial_layout.clone()),
            window_width,
            window_height,
            dock_left,
            dock_right,
            base_y: initial_layout.window_origin.y,
            last_poll_time: now,
            last_dock_refresh: now,
            #[cfg(target_os = "macos")]
            last_display_location_revision: tray::display_location_revision(),
            dock_autohide_probe: Some(initial_layout.dock_autohide),
            last_dock_autohide_probe: now,
            dock_autohide_probe_result: Arc::new(Mutex::new(None)),
            dock_autohide_probe_refreshing: Arc::new(Mutex::new(false)),
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
            cx_drag_offset: egui::Vec2::ZERO,
            cx_is_dragging: false,
            cx_snap_back_start: None,
            cx_last_cat_rect: egui::Rect::NOTHING,
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
    fn poll_claude_state(&mut self, cc_on: bool, cx_on: bool) -> Vec<String> {
        let entries = logger::read_recent_entries(200).unwrap_or_default();
        let mut new_agents = Vec::new();

        // 检测 subagent 生命周期事件
        for entry in &entries {
            // 跳过已关闭来源的事件
            if entry.source == "cx" && !cx_on {
                continue;
            }
            if entry.source != "cx" && !cc_on {
                continue;
            }
            let is_new = match &self.last_event_time {
                Some(t) => entry.timestamp > *t,
                None => false,
            };
            if !is_new {
                continue;
            }

            if entry.event_type == "SubagentStart" {
                if let Some(aid) = entry
                    .raw
                    .get("agent_id")
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
                if let Some(aid) = entry
                    .raw
                    .get("agent_id")
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
        let mut had_active_in_new_cx_events = false;
        for entry in &entries {
            if entry.source == "cx" && !cx_on {
                continue;
            }
            if entry.source != "cx" && !cc_on {
                continue;
            }
            let is_new = match &self.last_event_time {
                Some(t) => entry.timestamp > *t,
                None => true,
            };
            if !is_new {
                continue;
            }
            let is_subagent = entry
                .raw
                .get("agent_type")
                .and_then(|v| v.as_str())
                .is_some();
            if is_subagent {
                continue;
            }
            if entry.event_type == "ConfigChange" || entry.event_type == "InstructionsLoaded" {
                continue;
            }
            if event_type_to_state(&entry.event_type) == ClaudeState::Active {
                if entry.source == "cx" {
                    had_active_in_new_cx_events = true;
                } else {
                    had_active_in_new_events = true;
                }
            }
        }

        // 找最后一条 cc 主代理事件来决定主猫状态（排除 cx 事件）
        let last_non_subagent = if cc_on {
            entries.iter().rev().find(|e| {
                e.source != "cx"
                    && e.event_type != "ConfigChange"
                    && e.event_type != "InstructionsLoaded"
                    && e.raw.get("agent_type").and_then(|v| v.as_str()).is_none()
            })
        } else {
            None
        };
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
                                self.main_cat.notification_text = Some("等你回答".to_string());
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

        // ---- 计算 pending PermissionRequest 数量（按 source 分别统计） ----
        {
            let mut cc_last: HashMap<(String, String), &str> = HashMap::new();
            let mut cx_last: HashMap<(String, String), &str> = HashMap::new();
            for entry in &entries {
                let agent_id = entry
                    .raw
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let key = (entry.session_id.clone(), agent_id);
                if entry.source == "cx" {
                    if cx_on {
                        cx_last.insert(key, &entry.event_type);
                    }
                } else {
                    if cc_on {
                        cc_last.insert(key, &entry.event_type);
                    }
                }
            }
            self.main_cat.pending_permission_count = cc_last
                .values()
                .filter(|&&et| et == "PermissionRequest")
                .count();
            self.cx_main_cat.pending_permission_count = cx_last
                .values()
                .filter(|&&et| et == "PermissionRequest")
                .count();
        }

        // ---- cx 主猫状态 ----
        let last_cx_event = if cx_on {
            entries.iter().rev().find(|e| {
                e.source == "cx"
                    && e.event_type != "ConfigChange"
                    && e.event_type != "InstructionsLoaded"
                    && e.raw.get("agent_type").and_then(|v| v.as_str()).is_none()
            })
        } else {
            None
        };
        let cx_new_state = if let Some(entry) = last_cx_event {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&entry.timestamp) {
                let age = chrono::Local::now().signed_duration_since(ts);
                if age > chrono::Duration::minutes(5) {
                    ClaudeState::Offline
                } else {
                    // 检测 cx elicitation 通知
                    if entry.event_type == "Notification" {
                        if let Some(raw) = entry.raw.get("notification_type") {
                            if raw.as_str() == Some("elicitation_dialog") {
                                self.cx_main_cat.notification_text = Some("等你回答".to_string());
                                self.cx_main_cat.notification_expire =
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
        if current_event_time == self.last_event_time
            && new_state == self.main_cat.claude_state
            && cx_new_state == self.cx_main_cat.claude_state
        {
            return new_agents;
        }
        self.last_event_time = current_event_time;

        // ---- cc 主猫状态变化处理 ----
        if new_state != self.main_cat.claude_state {
            if self.main_cat.transition_anim.is_some() {
                self.main_cat.pending_state = Some(new_state);
            } else {
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
            }
        } else if had_active_in_new_events
            && new_state != ClaudeState::Active
            && self.main_cat.transition_anim.is_none()
        {
            self.main_cat.transition_anim = Some(ANIM_POUNCE);
            self.main_cat.pending_state = Some(new_state);
            self.main_cat.switch_to_animation(ANIM_POUNCE);
        }

        // ---- cx 主猫状态变化处理 ----
        if cx_new_state != self.cx_main_cat.claude_state {
            if self.cx_main_cat.transition_anim.is_some() {
                self.cx_main_cat.pending_state = Some(cx_new_state);
            } else {
                let old = self.cx_main_cat.claude_state;
                self.cx_main_cat.claude_state = cx_new_state;

                if old == ClaudeState::Offline
                    && (cx_new_state == ClaudeState::Idle || cx_new_state == ClaudeState::Active)
                {
                    self.cx_main_cat.transition_anim = Some(ANIM_STRETCH);
                    self.cx_main_cat.pending_state = Some(cx_new_state);
                    self.cx_main_cat.switch_to_animation(ANIM_STRETCH);
                } else if old == ClaudeState::Active
                    && (cx_new_state == ClaudeState::Idle || cx_new_state == ClaudeState::Offline)
                {
                    self.cx_main_cat.transition_anim = Some(ANIM_POUNCE);
                    self.cx_main_cat.pending_state = Some(cx_new_state);
                    self.cx_main_cat.switch_to_animation(ANIM_POUNCE);
                } else if had_active_in_new_cx_events && cx_new_state != ClaudeState::Active {
                    self.cx_main_cat.transition_anim = Some(ANIM_POUNCE);
                    self.cx_main_cat.pending_state = Some(cx_new_state);
                    self.cx_main_cat.switch_to_animation(ANIM_POUNCE);
                } else {
                    self.cx_main_cat.switch_to_state_animation(cx_new_state);
                }
            }
        } else if had_active_in_new_cx_events
            && cx_new_state != ClaudeState::Active
            && self.cx_main_cat.transition_anim.is_none()
        {
            self.cx_main_cat.transition_anim = Some(ANIM_POUNCE);
            self.cx_main_cat.pending_state = Some(cx_new_state);
            self.cx_main_cat.switch_to_animation(ANIM_POUNCE);
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

    /// 启动一次非阻塞 Dock 边界刷新任务。
    ///
    /// 语义与边界：
    /// - 仅负责提交刷新任务，不直接更新窗口位置；实际应用由 `apply_dock_result` 完成。
    /// - 若已有刷新任务在跑，直接返回，避免并发刷新导致的状态抖动。
    ///
    /// 入参：
    /// - `&self`：读取刷新状态并提交后台任务；不直接修改 UI。
    ///
    /// 返回：
    /// - `true`：成功提交了一次新的后台刷新任务。
    /// - `false`：已有刷新任务进行中，本次未提交。
    ///
    /// 错误处理与失败场景：
    /// - 屏幕采样失败时会跳过本轮刷新，并在任务结束后恢复 `dock_refreshing` 标记。
    ///
    /// 关键副作用：
    /// - 在主线程读取 `NSScreen`（避免跨线程访问 AppKit）。
    /// - 启动后台线程计算 Dock 边界并回写共享结果。
    fn start_dock_refresh_bg(&self) -> bool {
        let mut refreshing = self.dock_refreshing.lock().unwrap();
        if *refreshing {
            return false; // 已有后台任务在跑
        }
        *refreshing = true;

        let preferred_anchor_screen_id = self
            .applied_layout
            .as_ref()
            .map(|layout| layout.anchor_screen_id.clone());
        #[cfg(target_os = "macos")]
        let display_location_mode = crate::tray::current_display_location_mode();

        // AppKit 需主线程调用：这里只采样屏幕快照，后台线程仅做 Dock 边界计算。
        #[cfg(target_os = "macos")]
        let screen_info = get_macos_screen_info();

        let result_arc = Arc::clone(&self.dock_result);
        let refreshing_arc = Arc::clone(&self.dock_refreshing);

        std::thread::spawn(move || {
            #[cfg(target_os = "macos")]
            {
                if let Some(screens) = screen_info {
                    if let Some(fallback_screen) = main_screen_snapshot(&screens) {
                        let dock_sample = get_dock_sample_for_display_location(
                            &screens,
                            fallback_screen,
                            preferred_anchor_screen_id.as_deref(),
                            display_location_mode,
                        );
                        if dock_sample.walk_bounds.width > 0.0 {
                            if let Some(layout) = compute_applied_layout_for_dock_sample(
                                &screens,
                                fallback_screen,
                                &dock_sample,
                            ) {
                                let mut result = result_arc.lock().unwrap();
                                *result = Some(DockBoundsResult { layout });
                            }
                        }
                    }
                }
            }
            let mut refreshing = refreshing_arc.lock().unwrap();
            *refreshing = false;
        });
        true
    }

    /// 从后台结果应用 Dock 完整布局，返回是否需要窗口重摆。
    ///
    /// 语义与边界：
    /// - 仅在存在后台结果时更新状态；无结果时返回 `false`。
    /// - 布局变化判断依赖 `layout_changed`，由调用方决定是否发送窗口命令。
    ///
    /// 入参：
    /// - `&mut self`：会同步更新 `window_width/dock_left/dock_right/base_y` 与布局快照。
    ///
    /// 返回：
    /// - `true`：布局发生变化，建议重设窗口尺寸与位置。
    /// - `false`：布局未变或无新结果。
    ///
    /// 错误处理与失败场景：
    /// - 不返回错误；共享状态加锁失败时 panic（保持当前实现风格）。
    ///
    /// 关键副作用：
    /// - 仅在布局确实变化时，才会推进已应用布局并钳位猫的 `x_offset`。
    fn apply_dock_result(&mut self) -> bool {
        let result = {
            let mut lock = self.dock_result.lock().unwrap();
            lock.take()
        };
        if let Some(bounds) = result {
            if let Some(applied_layout) =
                resolve_next_applied_layout(self.applied_layout.as_ref(), &bounds.layout)
            {
                self.dock_left = applied_layout.window_origin.x;
                self.window_width = applied_layout.window_width;
                self.dock_right = self.dock_left + self.window_width;
                self.base_y = applied_layout.window_origin.y;
                self.dock_autohide_probe = Some(applied_layout.dock_autohide);
                self.applied_layout = Some(applied_layout);

                // 钳位所有猫（内联以避免借用冲突）
                let ww = self.window_width;
                {
                    let max_x = (ww - self.main_cat.max_width).max(0.0);
                    if self.main_cat.x_offset < 0.0 {
                        self.main_cat.x_offset = 0.0;
                    }
                    if self.main_cat.x_offset > max_x {
                        self.main_cat.x_offset = max_x;
                    }
                }
                {
                    let max_x = (ww - self.cx_main_cat.max_width).max(0.0);
                    if self.cx_main_cat.x_offset < 0.0 {
                        self.cx_main_cat.x_offset = 0.0;
                    }
                    if self.cx_main_cat.x_offset > max_x {
                        self.cx_main_cat.x_offset = max_x;
                    }
                }
                for mc in &mut self.mini_cats {
                    let max_x = (ww - mc.max_width).max(0.0);
                    if mc.x_offset < 0.0 {
                        mc.x_offset = 0.0;
                    }
                    if mc.x_offset > max_x {
                        mc.x_offset = max_x;
                    }
                }

                return true;
            }
        }
        false
    }

    /// 合并后台自动隐藏探测结果到本地缓存。
    ///
    /// 语义与边界：
    /// - 仅消费后台线程写回的最新值，不触发系统命令调用。
    /// - 无结果时保持当前缓存不变。
    ///
    /// 入参：
    /// - `&mut self`：会更新 `dock_autohide_probe`。
    ///
    /// 返回：
    /// - 无。
    ///
    /// 错误处理与失败场景：
    /// - 不返回错误；共享状态加锁失败时 panic（保持当前实现风格）。
    ///
    /// 关键副作用：
    /// - 无。
    fn apply_dock_autohide_probe_result(&mut self) {
        let probe_result = {
            let mut lock = self.dock_autohide_probe_result.lock().unwrap();
            lock.take()
        };
        if let Some(autohide) = probe_result {
            self.dock_autohide_probe = Some(autohide);
        }
    }

    /// 按固定采样周期启动后台 Dock 自动隐藏探测。
    ///
    /// 语义与边界：
    /// - 该探测仅用于刷新调度节流，不直接触发布局应用。
    /// - 仅在当前布局仍为非自动隐藏时启用，避免与已有 250ms Dock 刷新链路重复。
    /// - 采样间隔固定为 250ms，并在后台线程执行系统命令，避免阻塞 UI 线程。
    ///
    /// 入参：
    /// - `now`：当前时间戳，用于控制采样节流。
    ///
    /// 返回：
    /// - 无。
    ///
    /// 错误处理与失败场景：
    /// - macOS 探测失败时沿用 `dock_autohide_enabled` 的 `false` 兜底语义。
    ///
    /// 关键副作用：
    /// - 在后台线程调用 `defaults read com.apple.dock autohide`。
    fn start_dock_autohide_probe_bg(&mut self, now: Instant) {
        if !should_run_fast_autohide_probe(self.applied_layout.as_ref()) {
            return;
        }
        if now.duration_since(self.last_dock_autohide_probe) < Duration::from_millis(250) {
            return;
        }

        let mut refreshing = self.dock_autohide_probe_refreshing.lock().unwrap();
        if *refreshing {
            return;
        }
        *refreshing = true;
        self.last_dock_autohide_probe = now;
        drop(refreshing);

        #[cfg(target_os = "macos")]
        {
            let probe_result_arc = Arc::clone(&self.dock_autohide_probe_result);
            let refreshing_arc = Arc::clone(&self.dock_autohide_probe_refreshing);
            std::thread::spawn(move || {
                let autohide = dock_autohide_enabled();
                let mut probe_result = probe_result_arc.lock().unwrap();
                *probe_result = Some(autohide);
                let mut probe_refreshing = refreshing_arc.lock().unwrap();
                *probe_refreshing = false;
            });
            return;
        }

        #[cfg(not(target_os = "macos"))]
        {
            let mut probe_refreshing = self.dock_autohide_probe_refreshing.lock().unwrap();
            *probe_refreshing = false;
        }
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
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    self.window_width,
                    self.window_height,
                )));
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
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
                    // 大猫居中
                    self.main_cat.x_offset = (self.window_width - self.main_cat.max_width) / 2.0;
                    self.main_cat.last_move_time = now;
                }
            } else if self.position_phase == 6 {
                // Phase 6: 初始化窗口外观
                setup_window_appearance();
            }
            return; // 定位阶段不绘制
        }

        // ---- 读取 cc/cx 显示开关 ----
        #[cfg(target_os = "macos")]
        let (cc_on, cx_on) = (
            tray::CC_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
            tray::CX_ENABLED.load(std::sync::atomic::Ordering::Relaxed),
        );
        #[cfg(not(target_os = "macos"))]
        let (cc_on, cx_on) = (true, true);

        // ---- 每秒轮询事件 + 每 500ms 检查 pending approval ----
        if now.duration_since(self.last_poll_time) > Duration::from_millis(500) {
            if now.duration_since(self.last_poll_time) > Duration::from_secs(1) {
                // 轮询事件，收集需要新建的 mini cat ids
                let new_agent_ids = self.poll_claude_state(cc_on, cx_on);
                // 在事件循环之后创建 CatEntity，避免借用冲突
                for aid in new_agent_ids {
                    let start_x = if aid.starts_with("cx:") {
                        self.cx_main_cat.x_offset
                    } else {
                        self.main_cat.x_offset
                    };
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
        }

        // ---- 后台 Dock 刷新（自动隐藏时加速，不阻塞 UI） ----
        self.apply_dock_autohide_probe_result();
        self.start_dock_autohide_probe_bg(now);
        #[cfg(target_os = "macos")]
        {
            let revision = tray::display_location_revision();
            if revision != self.last_display_location_revision && self.start_dock_refresh_bg() {
                self.last_display_location_revision = revision;
                self.last_dock_refresh = now;
            }
        }
        let dock_refresh_interval =
            dock_refresh_interval_for_tick(self.applied_layout.as_ref(), self.dock_autohide_probe);
        if now.duration_since(self.last_dock_refresh) > dock_refresh_interval {
            self.last_dock_refresh = now;
            self.start_dock_refresh_bg();
        }
        // 检查后台结果并应用
        let bounds_changed = self.apply_dock_result();
        if bounds_changed {
            self.window_height = self.main_cat.max_height + 22.0;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                self.window_width,
                self.window_height,
            )));
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
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                    self.dock_left,
                    y,
                )));
            }
        }

        // ---- 托盘图标动画 (每 250ms) ----
        #[cfg(target_os = "macos")]
        {
            if now.duration_since(self.tray_last_frame_time) >= Duration::from_millis(250) {
                self.tray_last_frame_time = now;
                // 合并 cc/cx 状态：任一活跃则活跃，全部 Offline 才睡觉
                let combined_state = {
                    let cc_st = if cc_on {
                        self.main_cat.claude_state
                    } else {
                        ClaudeState::Offline
                    };
                    let cx_st = if cx_on {
                        self.cx_main_cat.claude_state
                    } else {
                        ClaudeState::Offline
                    };
                    if cc_st == ClaudeState::Active || cx_st == ClaudeState::Active {
                        ClaudeState::Active
                    } else if cc_st == ClaudeState::Idle || cx_st == ClaudeState::Idle {
                        ClaudeState::Idle
                    } else {
                        ClaudeState::Offline
                    }
                };
                tray::sync_tray_state(&mut self.tray_anim_state, combined_state);
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
                                    && (target == ClaudeState::Idle
                                        || target == ClaudeState::Offline)
                                {
                                    cat.transition_anim = Some(ANIM_POUNCE);
                                    cat.pending_state = Some(target);
                                    cat.switch_to_animation(ANIM_POUNCE);
                                } else if old == ClaudeState::Offline
                                    && (target == ClaudeState::Idle
                                        || target == ClaudeState::Active)
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

        // ---- 推进 cx 主猫动画帧（拖拽/下落中暂停） ----
        let cx_suspended = self.cx_is_dragging || self.cx_snap_back_start.is_some();
        if cx_on && !cx_suspended {
            let cat = &mut self.cx_main_cat;
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
                                    && (target == ClaudeState::Idle
                                        || target == ClaudeState::Offline)
                                {
                                    cat.transition_anim = Some(ANIM_POUNCE);
                                    cat.pending_state = Some(target);
                                    cat.switch_to_animation(ANIM_POUNCE);
                                } else if old == ClaudeState::Offline
                                    && (target == ClaudeState::Idle
                                        || target == ClaudeState::Active)
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

        // ---- 更新 cx 主猫位置（拖拽/下落中暂停） ----
        if cx_on && !cx_suspended {
            let cat = &mut self.cx_main_cat;
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
            self.cx_main_cat.last_move_time = now;
        }

        // ---- 迷你猫超时兜底：10 分钟未收到 SubagentStop 则自动返回 ----
        for mc in &mut self.mini_cats {
            if !mc.returning && now.duration_since(mc.spawn_time) > Duration::from_secs(600) {
                mc.returning = true;
            }
        }

        // ---- 更新迷你猫位置 ----
        let ww = self.window_width;
        let cc_center_x = self.main_cat.x_offset + self.main_cat.max_width / 2.0;
        let cx_center_x = self.cx_main_cat.x_offset + self.cx_main_cat.max_width / 2.0;
        for mc in &mut self.mini_cats {
            let move_speed = mc.animations[mc.state.current_anim].move_speed;
            if move_speed > 0.0 {
                let dt = now.duration_since(mc.last_move_time).as_secs_f32();
                mc.last_move_time = now;

                if mc.returning {
                    // returning：朝对应主猫中心跑
                    let target_center = if mc.id.starts_with("cx:") {
                        cx_center_x
                    } else {
                        cc_center_x
                    };
                    let mc_center = mc.x_offset + mc.max_width / 2.0;
                    let diff = target_center - mc_center;
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
                    if !mc.returning {
                        mc.move_direction = 1.0;
                    }
                }
                if mc.x_offset > max_x {
                    mc.x_offset = max_x;
                    if !mc.returning {
                        mc.move_direction = -1.0;
                    }
                }
            } else {
                mc.last_move_time = now;
            }
        }
        // 到达主猫身边的 returning 猫：删除
        self.mini_cats.retain(|mc| {
            if !mc.returning {
                return true;
            }
            let target_center = if mc.id.starts_with("cx:") {
                cx_center_x
            } else {
                cc_center_x
            };
            let mc_center = mc.x_offset + mc.max_width / 2.0;
            (mc_center - target_center).abs() > 5.0
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

        // ---- cx 拖拽下落动画 ----
        if cx_on {
            if let Some((start_time, start_y)) = self.cx_snap_back_start {
                let elapsed = now.duration_since(start_time).as_secs_f32();
                let duration = 0.25;
                if elapsed >= duration {
                    self.cx_main_cat.x_offset = (self.cx_main_cat.x_offset + self.cx_drag_offset.x)
                        .clamp(
                            0.0,
                            (self.window_width - self.cx_main_cat.max_width).max(0.0),
                        );
                    self.cx_drag_offset = egui::Vec2::ZERO;
                    self.cx_snap_back_start = None;
                    self.cx_main_cat.state.last_frame_time = now;
                } else {
                    let t = elapsed / duration;
                    let ease = t * t;
                    self.cx_drag_offset.y = start_y * (1.0 - ease);
                }
            }
        }

        // ---- 动态鼠标穿透（仅当鼠标在猫精灵上时关闭穿透） ----
        let either_dragging = (cc_on && self.is_dragging) || (cx_on && self.cx_is_dragging);
        let cc_rect = if cc_on {
            self.last_cat_rect
        } else {
            egui::Rect::NOTHING
        };
        let cx_rect = if cx_on {
            self.cx_last_cat_rect
        } else {
            egui::Rect::NOTHING
        };
        let cat_rects: Vec<egui::Rect> = [cc_rect, cx_rect]
            .iter()
            .copied()
            .filter(|r| *r != egui::Rect::NOTHING)
            .collect();
        update_mouse_passthrough(&cat_rects, either_dragging);

        // ---- 绘制 ----
        let panel_frame = egui::Frame::NONE.fill(egui::Color32::TRANSPARENT);

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                let available = ui.available_rect_before_wrap();

                // 先画迷你猫（在下层）
                for mc in self.mini_cats.iter().filter(|mc| {
                    if mc.id.starts_with("cx:") {
                        cx_on
                    } else {
                        cc_on
                    }
                }) {
                    let f = &mc.animations[mc.state.current_anim].frames[mc.state.current_frame];
                    let x = available.min.x + mc.x_offset + (mc.max_width - f.width) / 2.0;
                    let y = available.max.y - f.height - f.bottom_offset;

                    let rect =
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(f.width, f.height));

                    let move_speed = mc.animations[mc.state.current_anim].move_speed;
                    let uv = if mc.move_direction < 0.0 && move_speed > 0.0 {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                    };

                    ui.painter()
                        .image(f.texture.id(), rect, uv, egui::Color32::WHITE);
                }

                // 再画大猫（在上层）+ 拖拽交互
                if cc_on {
                    let cat = &self.main_cat;
                    let f = &cat.animations[cat.state.current_anim].frames[cat.state.current_frame];
                    let base_x = available.min.x + cat.x_offset + (cat.max_width - f.width) / 2.0;
                    let base_y = available.max.y - f.height - f.bottom_offset;

                    // 应用拖拽偏移，并约束在窗口区域内
                    let x = (base_x + self.drag_offset.x)
                        .clamp(available.min.x, available.max.x - f.width);
                    let y = (base_y + self.drag_offset.y)
                        .clamp(available.min.y, available.max.y - f.height);

                    let rect =
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(f.width, f.height));

                    // 保存猫矩形供下帧鼠标穿透检测使用
                    self.last_cat_rect = rect;

                    let move_speed = cat.animations[cat.state.current_anim].move_speed;
                    let uv = if cat.move_direction < 0.0 && move_speed > 0.0 {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                    };

                    ui.painter()
                        .image(f.texture.id(), rect, uv, egui::Color32::WHITE);

                    // ---- 拖拽交互 ----
                    let drag_response =
                        ui.interact(rect, egui::Id::new("main_cat_drag"), egui::Sense::drag());
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

                    // 大猫上方的气泡 -- PermissionRequest 计数
                    if cat.pending_permission_count > 0 {
                        let bubble_w = EXPANDED_W;
                        let bubble_x = available.min.x
                            + cat.x_offset
                            + (cat.max_width - bubble_w) / 2.0
                            + self.drag_offset.x;
                        let bubble_y = y - 22.0;
                        let bubble_rect = egui::Rect::from_min_size(
                            egui::pos2(
                                bubble_x.max(available.min.x),
                                bubble_y.max(available.min.y),
                            ),
                            egui::vec2(bubble_w, 18.0),
                        );
                        ui.painter().rect_filled(
                            bubble_rect,
                            4.0,
                            egui::Color32::from_rgba_unmultiplied(245, 243, 240, 230),
                        );
                        ui.painter().rect_stroke(
                            bubble_rect,
                            4.0,
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 40)),
                            epaint::StrokeKind::Outside,
                        );
                        let label = format!("需要人工介入 ({})", cat.pending_permission_count);
                        ui.painter().text(
                            bubble_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            &label,
                            egui::FontId::proportional(10.0),
                            egui::Color32::from_rgba_unmultiplied(40, 40, 40, 255),
                        );
                    }

                    // Elicitation 通知气泡
                    if let Some(ref text) = cat.notification_text {
                        if now < cat.notification_expire {
                            let bubble_w = EXPANDED_W;
                            let bubble_x = available.min.x
                                + cat.x_offset
                                + (cat.max_width - bubble_w) / 2.0
                                + self.drag_offset.x;
                            let bubble_y = y - 22.0;
                            let bubble_rect = egui::Rect::from_min_size(
                                egui::pos2(
                                    bubble_x.max(available.min.x),
                                    bubble_y.max(available.min.y),
                                ),
                                egui::vec2(bubble_w, 18.0),
                            );
                            ui.painter().rect_filled(
                                bubble_rect,
                                4.0,
                                egui::Color32::from_rgba_unmultiplied(245, 243, 240, 230),
                            );
                            ui.painter().rect_stroke(
                                bubble_rect,
                                4.0,
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

                // 画 cx 主猫 + 拖拽交互 (cx_on 时才画)
                if cx_on {
                    let cat = &self.cx_main_cat;
                    let f = &cat.animations[cat.state.current_anim].frames[cat.state.current_frame];
                    let base_x = available.min.x + cat.x_offset + (cat.max_width - f.width) / 2.0;
                    let base_y = available.max.y - f.height - f.bottom_offset;

                    let x = (base_x + self.cx_drag_offset.x)
                        .clamp(available.min.x, available.max.x - f.width);
                    let y = (base_y + self.cx_drag_offset.y)
                        .clamp(available.min.y, available.max.y - f.height);

                    let rect =
                        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(f.width, f.height));

                    self.cx_last_cat_rect = rect;

                    let move_speed = cat.animations[cat.state.current_anim].move_speed;
                    let uv = if cat.move_direction < 0.0 && move_speed > 0.0 {
                        egui::Rect::from_min_max(egui::pos2(1.0, 0.0), egui::pos2(0.0, 1.0))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                    };

                    ui.painter()
                        .image(f.texture.id(), rect, uv, egui::Color32::WHITE);

                    // 拖拽交互
                    let drag_response =
                        ui.interact(rect, egui::Id::new("cx_cat_drag"), egui::Sense::drag());
                    if drag_response.dragged() {
                        self.cx_is_dragging = true;
                        self.cx_snap_back_start = None;
                        self.cx_drag_offset += drag_response.drag_delta();
                        let min_ox = available.min.x - base_x;
                        let max_ox = available.max.x - f.width - base_x;
                        let min_oy = available.min.y - base_y;
                        let max_oy = 0.0_f32;
                        self.cx_drag_offset.x = self.cx_drag_offset.x.clamp(min_ox, max_ox);
                        self.cx_drag_offset.y = self.cx_drag_offset.y.clamp(min_oy, max_oy);
                    }
                    if drag_response.drag_stopped() {
                        self.cx_is_dragging = false;
                        if self.cx_drag_offset.y.abs() > 0.5 {
                            self.cx_snap_back_start = Some((now, self.cx_drag_offset.y));
                        } else {
                            self.cx_drag_offset.y = 0.0;
                        }
                    }

                    // zzz
                    if cat.state.current_anim == ANIM_SLEEP {
                        let t_global = self.app_start.elapsed().as_secs_f32();
                        let cycle = 2.4_f32;
                        let stagger = 0.8_f32;
                        let head_x = rect.center().x + rect.width() * 0.25;
                        let head_y = rect.min.y + 4.0;

                        for i in 0..3u32 {
                            let phase = (t_global + i as f32 * stagger) % cycle;
                            let progress = phase / cycle;
                            let float_y = head_y - progress * 30.0;
                            let drift_x = head_x + i as f32 * 4.0 + progress * 8.0;
                            let alpha = if progress < 0.1 {
                                progress / 0.1
                            } else if progress < 0.7 {
                                1.0
                            } else {
                                1.0 - (progress - 0.7) / 0.3
                            };
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

                    // cx PermissionRequest 气泡
                    if cat.pending_permission_count > 0 {
                        let bubble_w = EXPANDED_W;
                        let bubble_x = available.min.x
                            + cat.x_offset
                            + (cat.max_width - bubble_w) / 2.0
                            + self.cx_drag_offset.x;
                        let bubble_y = y - 22.0;
                        let bubble_rect = egui::Rect::from_min_size(
                            egui::pos2(
                                bubble_x.max(available.min.x),
                                bubble_y.max(available.min.y),
                            ),
                            egui::vec2(bubble_w, 18.0),
                        );
                        ui.painter().rect_filled(
                            bubble_rect,
                            4.0,
                            egui::Color32::from_rgba_unmultiplied(245, 243, 240, 230),
                        );
                        ui.painter().rect_stroke(
                            bubble_rect,
                            4.0,
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(40, 40, 40)),
                            epaint::StrokeKind::Outside,
                        );
                        let label = format!("需要人工介入 ({})", cat.pending_permission_count);
                        ui.painter().text(
                            bubble_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            &label,
                            egui::FontId::proportional(10.0),
                            egui::Color32::from_rgba_unmultiplied(40, 40, 40, 255),
                        );
                    }

                    // cx Notification 气泡
                    if let Some(ref text) = cat.notification_text {
                        if now < cat.notification_expire {
                            let bubble_w = EXPANDED_W;
                            let bubble_x = available.min.x
                                + cat.x_offset
                                + (cat.max_width - bubble_w) / 2.0
                                + self.cx_drag_offset.x;
                            let bubble_y = y - 22.0;
                            let bubble_rect = egui::Rect::from_min_size(
                                egui::pos2(
                                    bubble_x.max(available.min.x),
                                    bubble_y.max(available.min.y),
                                ),
                                egui::vec2(bubble_w, 18.0),
                            );
                            ui.painter().rect_filled(
                                bubble_rect,
                                4.0,
                                egui::Color32::from_rgba_unmultiplied(245, 243, 240, 230),
                            );
                            ui.painter().rect_stroke(
                                bubble_rect,
                                4.0,
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
                if let Some(ref _text) = self.cx_main_cat.notification_text {
                    if now >= self.cx_main_cat.notification_expire {
                        self.cx_main_cat.notification_text = None;
                    }
                }
            });
    }
}

// ============================================================
// macOS 平台函数
// ============================================================

/// 根据 Dock 中点定位其所属屏幕。
///
/// 语义与边界：
/// - 输入点为全局 point 坐标。
/// - 仅按 `frame` 判断归属，不考虑 `visible_frame`。
///
/// 入参：
/// - `screens`：全量屏幕快照。
/// - `dock_mid_x`/`dock_mid_y`：Dock 中点全局坐标。
///
/// 返回：
/// - `Some(&MacScreenSnapshot)`：命中屏幕。
/// - `None`：点不落在任何采样屏幕内。
#[cfg(target_os = "macos")]
fn find_anchor_screen<'a>(
    screens: &'a [MacScreenSnapshot],
    dock_mid_x: f32,
    dock_mid_y: f32,
) -> Option<&'a MacScreenSnapshot> {
    screens
        .iter()
        .find(|screen| screen.frame.contains_point(dock_mid_x, dock_mid_y))
}

/// 从屏幕快照中选择默认锚点屏幕。
///
/// 语义与边界：
/// - 优先使用 `is_main=true` 的屏幕。
/// - 若快照中没有主屏标记，则退化为最高缩放倍率的屏幕，保证稳定顺序。
#[cfg(target_os = "macos")]
fn main_screen_snapshot(screens: &[MacScreenSnapshot]) -> Option<&MacScreenSnapshot> {
    use std::cmp::Ordering;

    screens.iter().find(|screen| screen.is_main).or_else(|| {
        screens.iter().max_by(|lhs, rhs| {
            lhs.scale_factor
                .partial_cmp(&rhs.scale_factor)
                .unwrap_or(Ordering::Equal)
                .then_with(|| lhs.id.cmp(&rhs.id))
        })
    })
}

/// 按屏幕 ID 查找对应快照。
///
/// 语义与边界：
/// - 仅在同一轮快照数组中查找，不跨轮次缓存。
///
/// 入参：
/// - `screens`：待查找的屏幕快照集合。
/// - `screen_id`：目标屏幕标识。
///
/// 返回：
/// - `Some(&MacScreenSnapshot)`：找到对应屏幕。
/// - `None`：当前快照中不存在该 ID。
#[cfg(target_os = "macos")]
fn screen_by_id<'a>(
    screens: &'a [MacScreenSnapshot],
    screen_id: &str,
) -> Option<&'a MacScreenSnapshot> {
    screens.iter().find(|screen| screen.id == screen_id)
}

/// 按运行期显示器选择 ID 查找对应快照。
///
/// 语义与边界：
/// - `selection_id` 用于匹配用户手动选择的目标显示器。
/// - 仅在同一轮采样数组中查找，不跨轮次缓存对象引用。
///
/// 入参：
/// - `screens`：待查找的屏幕快照集合。
/// - `selection_id`：目标显示器的运行期选择标识。
///
/// 返回：
/// - `Some(&MacScreenSnapshot)`：找到对应屏幕。
/// - `None`：当前快照中不存在该选择标识。
///
/// 错误处理与失败场景：
/// - 不返回错误；ID 缺失由调用方决定是否回退。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn screen_by_selection_id<'a>(
    screens: &'a [MacScreenSnapshot],
    selection_id: &str,
) -> Option<&'a MacScreenSnapshot> {
    screens
        .iter()
        .find(|screen| screen.selection_id == selection_id)
}

/// 解析 Dock 结果对应的锚点屏幕。
///
/// 语义与边界：
/// - 优先使用 `dock_bounds.anchor_screen_id` 命中的屏幕，确保 Dock 几何与基线来自同一块屏。
/// - 若当前快照找不到该 ID，则回退到调用方给定的 `fallback_screen`。
///
/// 入参：
/// - `screens`：同一轮采样得到的屏幕快照集合。
/// - `fallback_screen`：兜底屏幕（通常为主屏）。
/// - `dock_sample`：Dock 采样结果及其锚点屏幕 ID。
///
/// 返回：
/// - 与本次 Dock 结果一致的屏幕快照引用；找不到锚点时返回 `fallback_screen`。
///
/// 错误处理与失败场景：
/// - 不返回错误；ID 未命中时采用兜底策略，避免刷新路径中断。
///
/// 关键副作用：
/// - 无。
#[cfg(all(test, target_os = "macos"))]
fn resolve_dock_anchor_screen<'a>(
    screens: &'a [MacScreenSnapshot],
    fallback_screen: &'a MacScreenSnapshot,
    dock_sample: &DockPlacementSample,
) -> &'a MacScreenSnapshot {
    screen_by_id(screens, &dock_sample.dock_snapshot.anchor_screen_id).unwrap_or(fallback_screen)
}

/// 构造 side Dock 退化模式下的 Dock 采样结果。
///
/// 语义与边界：
/// - side Dock 固定使用 `DockPlacementMode::Floor`。
/// - 活动区域必须使用锚点屏幕的 `visible_frame`，避免进入侧边 Dock 占用区。
///
/// 入参：
/// - `anchor_screen`：Dock 锚定屏幕快照。
/// - `autohide`：Dock 是否自动隐藏。
///
/// 返回：
/// - `DockPlacementSample`：Floor 模式统一输出。
///
/// 错误处理与失败场景：
/// - 不返回错误；调用方需确保 `anchor_screen` 来自当前采样结果。
#[cfg(target_os = "macos")]
fn build_floor_mode_sample(
    anchor_screen: &MacScreenSnapshot,
    autohide: bool,
) -> DockPlacementSample {
    let walk_bounds = anchor_screen.visible_frame.clone();
    DockPlacementSample {
        dock_snapshot: crate::cat_layout::DockSnapshot::side(anchor_screen.id.clone(), autohide),
        walk_bounds,
    }
}

/// 解析当前请求的显示位置模式在本轮屏幕快照下的有效值。
///
/// 语义与边界：
/// - `Auto` 始终保持不变。
/// - `Specific(selection_id)` 仅在当前快照仍能找到目标显示器时保留；否则回退为 `Auto`。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
/// - `requested_mode`：调用方当前请求的显示位置模式。
///
/// 返回：
/// - 当前快照下实际可用的显示位置模式。
///
/// 错误处理与失败场景：
/// - 不返回错误；目标显示器缺失时自动回退到 `Auto`。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn resolve_display_location_mode(
    screens: &[MacScreenSnapshot],
    requested_mode: DisplayLocationMode,
) -> DisplayLocationMode {
    match requested_mode {
        DisplayLocationMode::Auto => DisplayLocationMode::Auto,
        DisplayLocationMode::Specific(selection_id) => {
            if screen_by_selection_id(screens, &selection_id).is_some() {
                DisplayLocationMode::Specific(selection_id)
            } else {
                DisplayLocationMode::Auto
            }
        }
    }
}

/// 基于当前显示位置模式解析最终应使用的 Dock 采样结果。
///
/// 语义与边界：
/// - `Auto` 直接复用现有 Dock 自动跟随采样结果。
/// - `Specific(selection_id)` 且 Dock 已在该显示器上时，也复用自动采样结果。
/// - `Specific(selection_id)` 且 Dock 在其它显示器上时，退化为目标显示器的 `Floor` 模式，
///   令像素猫沿该显示器底边活动。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
/// - `auto_sample`：默认自动跟随 Dock 的采样结果。
/// - `display_location_mode`：当前已解析好的显示位置模式。
///
/// 返回：
/// - 本轮应继续用于布局计算的统一 Dock 采样结果。
///
/// 错误处理与失败场景：
/// - 若 `Specific(selection_id)` 在当前快照中找不到显示器，则退化为 `auto_sample`。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn resolve_display_location_dock_sample(
    screens: &[MacScreenSnapshot],
    auto_sample: &DockPlacementSample,
    display_location_mode: DisplayLocationMode,
) -> DockPlacementSample {
    match display_location_mode {
        DisplayLocationMode::Auto => auto_sample.clone(),
        DisplayLocationMode::Specific(selection_id) => {
            let selected_screen = match screen_by_selection_id(screens, &selection_id) {
                Some(screen) => screen,
                None => return auto_sample.clone(),
            };
            let dock_screen = screen_by_id(screens, &auto_sample.dock_snapshot.anchor_screen_id)
                .unwrap_or(selected_screen);

            if dock_screen.selection_id == selected_screen.selection_id {
                auto_sample.clone()
            } else {
                build_floor_mode_sample(selected_screen, auto_sample.dock_snapshot.autohide)
            }
        }
    }
}

/// 从屏幕快照构造托盘菜单使用的显示器选项列表。
///
/// 语义与边界：
/// - 若显示器名称重复，会按当前快照顺序追加 `#序号` 以保证菜单文案可区分。
/// - 仅构造 UI 展示所需信息，不保留几何字段。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
///
/// 返回：
/// - 托盘菜单可直接消费的显示器选项列表。
///
/// 错误处理与失败场景：
/// - 不返回错误；空输入返回空列表。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn display_choices_from_screens(screens: &[MacScreenSnapshot]) -> Vec<DisplayChoice> {
    let mut totals: HashMap<&str, usize> = HashMap::new();
    for screen in screens {
        *totals.entry(screen.name.as_str()).or_insert(0) += 1;
    }

    let mut occurrences: HashMap<&str, usize> = HashMap::new();
    screens
        .iter()
        .map(|screen| {
            let occurrence = occurrences.entry(screen.name.as_str()).or_insert(0);
            *occurrence += 1;
            let label = if totals.get(screen.name.as_str()).copied().unwrap_or(0) > 1 {
                format!("{} #{}", screen.name, *occurrence)
            } else {
                screen.name.clone()
            };

            DisplayChoice {
                selection_id: screen.selection_id.clone(),
                label,
                is_main: screen.is_main,
            }
        })
        .collect()
}

/// 返回当前 macOS 会话中的显示器菜单选项。
///
/// 语义与边界：
/// - 必须在主线程调用，因为内部会访问 `NSScreen`。
/// - 返回值仅描述当前这一刻的显示器列表，不做跨刷新缓存。
///
/// 返回：
/// - 当前可用于托盘“显示位置”菜单的显示器选项列表。
///
/// 错误处理与失败场景：
/// - 屏幕采样失败时返回空列表，由调用方决定是否隐藏菜单。
///
/// 关键副作用：
/// - 读取 AppKit 的 `NSScreen` 信息。
#[cfg(target_os = "macos")]
pub(crate) fn current_display_choices() -> Vec<DisplayChoice> {
    get_macos_screen_info()
        .map(|screens| display_choices_from_screens(&screens))
        .unwrap_or_default()
}

/// 依据 side Dock 朝向选择 Floor 模式的锚点屏幕。
///
/// 语义与边界：
/// - `orientation` 仅支持 `"left"` 和 `"right"`，其它值直接回退 `fallback_screen`。
/// - 通过 `visible_frame` 相对 `frame` 的水平内缩推断 Dock 所在屏：
///   - `left`：优先选择 `visible_frame.x > frame.x` 的屏幕。
///   - `right`：优先选择 `visible_frame.right < frame.right` 的屏幕。
/// - 同向命中多块屏时，优先选择内缩量最大的屏幕。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
/// - `fallback_screen`：兜底屏幕（通常是主屏）。
/// - `orientation`：Dock 朝向字符串。
///
/// 返回：
/// - Floor 模式使用的锚点屏幕快照。
///
/// 错误处理与失败场景：
/// - 不返回错误；无法识别朝向时回退兜底屏幕。
#[cfg(target_os = "macos")]
fn select_side_dock_anchor_screen<'a>(
    screens: &'a [MacScreenSnapshot],
    fallback_screen: &'a MacScreenSnapshot,
    orientation: &str,
) -> &'a MacScreenSnapshot {
    use std::cmp::Ordering;

    const SIDE_DOCK_INSET_EPSILON: f32 = 0.5;

    let select_max_inset =
        |extract_inset: fn(&MacScreenSnapshot) -> f32| -> Option<&MacScreenSnapshot> {
            screens
                .iter()
                .filter_map(|screen| {
                    let inset = extract_inset(screen);
                    if inset > SIDE_DOCK_INSET_EPSILON {
                        Some((screen, inset))
                    } else {
                        None
                    }
                })
                .max_by(|lhs, rhs| {
                    lhs.1
                        .partial_cmp(&rhs.1)
                        .unwrap_or(Ordering::Equal)
                        .then_with(|| lhs.0.id.cmp(&rhs.0.id))
                })
                .map(|(screen, _)| screen)
        };

    let left_inset =
        |screen: &MacScreenSnapshot| -> f32 { (screen.visible_frame.x - screen.frame.x).max(0.0) };
    let right_inset = |screen: &MacScreenSnapshot| -> f32 {
        let frame_right = screen.frame.x + screen.frame.width;
        let visible_right = screen.visible_frame.x + screen.visible_frame.width;
        (frame_right - visible_right).max(0.0)
    };

    match orientation {
        "left" => select_max_inset(left_inset).unwrap_or(fallback_screen),
        "right" => select_max_inset(right_inset).unwrap_or(fallback_screen),
        _ => fallback_screen,
    }
}

/// 依据 side Dock 朝向选择 Floor 模式锚点屏幕，并优先使用“上一次确认的锚点屏幕”作为兜底。
///
/// 语义与边界：
/// - 当 Dock 自动隐藏且处于隐藏态时，`visible_frame` 的水平内缩可能消失，导致
///   `select_side_dock_anchor_screen` 无法通过内缩推断 Dock 所在屏幕。
/// - 本函数允许调用方提供 `preferred_anchor_screen_id`（通常来自上一轮已应用布局），
///   使得“内缩消失”时依然优先保留已确认的副屏锚点。
/// - `preferred_anchor_screen_id` 仅用于兜底：当存在有效内缩命中时仍以命中结果为准。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
/// - `fallback_screen`：默认兜底屏幕（通常是主屏）。
/// - `preferred_anchor_screen_id`：上一次已确认的锚点屏幕 ID，可为空。
/// - `orientation`：Dock 朝向字符串，仅支持 `"left"` / `"right"`。
///
/// 返回：
/// - Floor 模式使用的锚点屏幕快照。
///
/// 错误处理与失败场景：
/// - 不返回错误；ID 不命中时会回退到 `fallback_screen`。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn select_side_dock_anchor_screen_with_preference<'a>(
    screens: &'a [MacScreenSnapshot],
    fallback_screen: &'a MacScreenSnapshot,
    preferred_anchor_screen_id: Option<&str>,
    orientation: &str,
) -> &'a MacScreenSnapshot {
    let fallback = preferred_anchor_screen_id
        .and_then(|screen_id| screen_by_id(screens, screen_id))
        .unwrap_or(fallback_screen);
    select_side_dock_anchor_screen(screens, fallback, orientation)
}

/// 基于锚点屏幕与水平边界估算 Bottom 模式 Dock 矩形。
///
/// 语义与边界：
/// - 仅用于非 AX 的估算路径。
/// - 高度优先使用 `visible_frame.y - frame.y` 推断；推断失败时退化为 20pt。
///
/// 入参：
/// - `anchor_screen`：Dock 锚定屏幕快照。
/// - `left`/`right`：Dock 可活动水平边界（全局 point）。
///
/// 返回：
/// - 估算得到的 Dock 矩形。
#[cfg(target_os = "macos")]
fn estimate_bottom_dock_frame(
    anchor_screen: &MacScreenSnapshot,
    left: f32,
    right: f32,
) -> crate::cat_layout::Rect {
    let width = (right - left).max(0.0);
    let inferred_height = (anchor_screen.visible_frame.y - anchor_screen.frame.y).max(0.0);
    let height = if inferred_height > 0.5 {
        inferred_height
    } else {
        20.0
    };
    let y = anchor_screen.visible_frame.y - height;
    crate::cat_layout::Rect::new(left, y, width, height)
}

/// 构造 Bottom 模式 Dock 采样结果。
///
/// 语义与边界：
/// - Bottom 模式保留 `dock_snapshot.dock_frame`，AX 路径优先使用真实矩形。
/// - `walk_bounds` 仅表达可活动水平范围，宽度由 `left/right` 决定。
///
/// 入参：
/// - `anchor_screen`：Dock 锚定屏幕快照。
/// - `bounds`：Bottom 模式水平边界及可选真实 Dock 矩形。
/// - `autohide`：Dock 是否自动隐藏。
///
/// 返回：
/// - `DockPlacementSample`：Bottom 模式统一输出。
#[cfg(target_os = "macos")]
fn build_bottom_mode_sample(
    anchor_screen: &MacScreenSnapshot,
    bounds: &DockHorizontalBounds,
    autohide: bool,
) -> DockPlacementSample {
    let dock_frame = bounds
        .dock_frame
        .clone()
        .unwrap_or_else(|| estimate_bottom_dock_frame(anchor_screen, bounds.left, bounds.right));
    let walk_bounds = crate::cat_layout::Rect::new(
        bounds.left,
        dock_frame.y,
        (bounds.right - bounds.left).max(0.0),
        dock_frame.height,
    );
    DockPlacementSample {
        dock_snapshot: crate::cat_layout::DockSnapshot::bottom_with_walk_bounds(
            anchor_screen.id.clone(),
            dock_frame,
            walk_bounds.clone(),
            autohide,
        ),
        walk_bounds,
    }
}

/// 读取 Dock 自动隐藏开关。
///
/// 语义与边界：
/// - 仅解析 `defaults read com.apple.dock autohide` 的结果。
/// - 解析失败时默认返回 `false`，保持历史行为稳定。
#[cfg(target_os = "macos")]
fn dock_autohide_enabled() -> bool {
    std::process::Command::new("defaults")
        .args(["read", "com.apple.dock", "autohide"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|value| value.trim().parse::<i32>().ok())
        .unwrap_or(0)
        != 0
}

/// 计算 winit/macOS 坐标系所依赖的“主显示器高度”。
///
/// 语义与边界：
/// - winit 在 macOS 下使用 CoreGraphics 的全局坐标：原点位于主显示器左上角，
///   Y 轴向下增长。
/// - AppKit / `NSScreen` 则使用主显示器左下角为原点、Y 轴向上增长的坐标系。
/// - 该函数返回主显示器高度，用于把 AppKit 的 `visible_frame.y` 换算到 winit
///   所需的“距离主显示器顶部”语义。
/// - 优先选择 `frame` 覆盖全局原点 `(0, 0)` 的屏幕；若找不到，再退回主屏标记。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
///
/// 返回：
/// - `Some(f32)`：主显示器高度（point）。
/// - `None`：输入为空。
///
/// 错误处理与失败场景：
/// - 不返回错误；仅用 `Option` 表达输入缺失。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn main_display_height(screens: &[MacScreenSnapshot]) -> Option<f32> {
    screens
        .iter()
        .find(|screen| screen.frame.contains_point(0.0, 0.0))
        .or_else(|| main_screen_snapshot(screens))
        .map(|screen| screen.frame.height)
}

/// 将 `visible_frame` 转换为当前窗口使用的“离主显示器顶部距离”语义。
///
/// 语义与边界：
/// - 返回值对齐 winit/macOS 的全局坐标系：相对主显示器顶部测量，Y 轴向下。
/// - 对位于主显示器上方的副屏，该值会自然变为 `<= 0`，避免窗口被错误地推到
///   另一块屏幕上。
/// - 对位于主显示器下方的副屏，该值会大于主显示器高度，保持纵向偏移正确。
///
/// 入参：
/// - `screens`：同一轮采样得到的全量屏幕快照。
/// - `screen`：Dock 所在锚点屏幕快照。
///
/// 返回：
/// - 基于主显示器顶部换算后的基线值（point）。
///
/// 错误处理与失败场景：
/// - 当 `screens` 为空时，退化为锚点屏幕高度，保持旧路径可用性。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn legacy_visible_bottom(screens: &[MacScreenSnapshot], screen: &MacScreenSnapshot) -> f32 {
    let top = main_display_height(screens).unwrap_or(screen.frame.height);
    top - screen.visible_frame.y
}

/// macOS: 获取 Dock 采样统一输出（Bottom/Floor）。
///
/// 语义与边界：
/// - 优先使用 AX 读取真实 Dock 边界；若失败则使用偏好配置估算。
/// - side Dock 返回 `Floor` 模式，活动范围使用 `visible_frame`。
/// - bottom Dock 返回 `Bottom` 模式，保留 `dock_snapshot.dock_frame`。
///
/// 入参：
/// - `screens`：当前采样到的全部屏幕快照（全局坐标）。
/// - `anchor_screen`：默认锚定屏幕（通常是主屏）。
///
/// 返回：
/// - `DockPlacementSample`：可直接用于运行时布局的统一结果。
///
/// 错误处理与失败场景：
/// - 本函数内部不返回错误；若 AX 不可用，会自动退化到估算路径。
#[cfg(target_os = "macos")]
fn get_dock_sample(
    screens: &[MacScreenSnapshot],
    anchor_screen: &MacScreenSnapshot,
) -> DockPlacementSample {
    get_dock_sample_for_display_location(screens, anchor_screen, None, DisplayLocationMode::Auto)
}

/// macOS: 获取 Dock 采样统一输出（Bottom/Floor），并允许注入“优先锚点屏幕”以稳定 side Dock 隐藏态回退。
///
/// 语义与边界：
/// - 与 `get_dock_sample` 相同，但允许调用方传入 `preferred_anchor_screen_id`：
///   - 仅在 side Dock 且无法通过 `visible_frame` 内缩推断屏幕时，才作为 fallback 生效；
///   - 典型用于“side Dock + autohide + Dock 在副屏”场景，避免隐藏态误回退到主屏。
///
/// 入参：
/// - `screens`：当前采样到的全部屏幕快照（全局坐标）。
/// - `anchor_screen`：默认锚定屏幕（通常是主屏）。
/// - `preferred_anchor_screen_id`：上一轮已确认锚点屏幕 ID，可为空。
///
/// 返回：
/// - `DockPlacementSample`：可直接用于运行时布局的统一结果。
///
/// 错误处理与失败场景：
/// - 本函数内部不返回错误；若 AX 不可用，会自动退化到估算路径。
#[cfg(target_os = "macos")]
fn get_dock_sample_with_preferred_anchor(
    screens: &[MacScreenSnapshot],
    anchor_screen: &MacScreenSnapshot,
    preferred_anchor_screen_id: Option<&str>,
) -> DockPlacementSample {
    let autohide = dock_autohide_enabled();

    // Dock 不在底部时，窗口铺满锚定屏幕宽度（保持现有 floor 行为）
    if let Ok(output) = std::process::Command::new("defaults")
        .args(&["read", "com.apple.dock", "orientation"])
        .output()
    {
        let orientation = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if orientation == "left" || orientation == "right" {
            let dock_screen = select_side_dock_anchor_screen_with_preference(
                screens,
                anchor_screen,
                preferred_anchor_screen_id,
                &orientation,
            );
            return build_floor_mode_sample(dock_screen, autohide);
        }
    }

    // 优先尝试 Accessibility API
    if let Some(bounds) = get_dock_bounds_ax(screens) {
        let dock_screen = screen_by_id(screens, &bounds.anchor_screen_id).unwrap_or(anchor_screen);
        return build_bottom_mode_sample(dock_screen, &bounds, autohide);
    }

    // Fallback: 估算
    let (left, right) = get_dock_bounds_estimate(anchor_screen.frame.x, anchor_screen.frame.width);
    let bounds = DockHorizontalBounds {
        anchor_screen_id: anchor_screen.id.clone(),
        left,
        right,
        dock_frame: None,
    };
    build_bottom_mode_sample(anchor_screen, &bounds, autohide)
}

/// 在默认 Dock 自动采样结果之上叠加显示位置模式。
///
/// 语义与边界：
/// - 先按现有逻辑生成“跟随 Dock”的自动采样结果。
/// - 再根据 `display_location_mode` 决定是否强制切到手动指定显示器的 floor 模式。
/// - 若手动指定的显示器当前已消失，则自动回退到 `Auto` 并同步更新运行期状态。
///
/// 入参：
/// - `screens`：当前采样到的全部屏幕快照（全局坐标）。
/// - `anchor_screen`：默认锚定屏幕（通常是主屏）。
/// - `preferred_anchor_screen_id`：上一轮已确认的 Dock 锚点屏幕 ID，可为空。
/// - `display_location_mode`：当前请求的显示位置模式。
///
/// 返回：
/// - 本轮最终应使用的统一 Dock 采样结果。
///
/// 错误处理与失败场景：
/// - 不返回错误；当手动目标显示器缺失时，会退回 `Auto` 路径。
///
/// 关键副作用：
/// - 目标显示器消失时，会把托盘中的运行期显示位置状态重置为 `Auto`。
#[cfg(target_os = "macos")]
fn get_dock_sample_for_display_location(
    screens: &[MacScreenSnapshot],
    anchor_screen: &MacScreenSnapshot,
    preferred_anchor_screen_id: Option<&str>,
    display_location_mode: DisplayLocationMode,
) -> DockPlacementSample {
    let auto_sample =
        get_dock_sample_with_preferred_anchor(screens, anchor_screen, preferred_anchor_screen_id);
    let resolved_mode = resolve_display_location_mode(screens, display_location_mode.clone());
    if resolved_mode != display_location_mode {
        crate::tray::set_display_location_mode(resolved_mode.clone());
    }

    resolve_display_location_dock_sample(screens, &auto_sample, resolved_mode)
}

/// 基于 Dock AXList 的原始几何值构造底部 Dock 边界。
///
/// 语义与边界：
/// - 输入值直接来自 Accessibility API 的 `AXPosition/AXSize`。
/// - `AXPosition.y` 在运行时会先换算到 AppKit 使用的全局坐标系，再参与锚屏判断；
///   这样上下堆叠多屏时不会把底部 Dock 误判到其它屏幕。
/// - 仅负责把原始几何换算为运行时使用的 `DockHorizontalBounds`，不处理权限与 AX
///   遍历流程。
/// - 若几何值无法映射到任何屏幕，返回 `None`，由调用方回退到其它路径。
///
/// 入参：
/// - `screens`：当前轮次的屏幕快照。
/// - `dock_left`：Dock AXList 的水平起点（point）。
/// - `dock_vertical_origin`：Dock AXList 返回的垂直基准值（point）。
/// - `dock_width`/`dock_height`：Dock AXList 尺寸（point）。
///
/// 返回：
/// - `Some(DockHorizontalBounds)`：成功识别锚点屏幕并得到水平活动范围。
/// - `None`：几何值不合法，或 Dock 无法归属到当前任一屏幕。
///
/// 错误处理与失败场景：
/// - 不抛异常；调用方用 `Option` 继续走非 AX 回退逻辑。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn dock_bounds_from_ax_list_metrics(
    screens: &[MacScreenSnapshot],
    dock_left: f32,
    dock_vertical_origin: f32,
    dock_width: f32,
    dock_height: f32,
) -> Option<DockHorizontalBounds> {
    let main_height = main_display_height(screens).unwrap_or(dock_height);
    let dock_right = dock_left + dock_width;
    let dock_bottom = main_height - dock_vertical_origin;

    // 用 AXList 高度推算圆角: squircle 圆角 ≈ 高度 × 0.27
    let corner_r = (dock_height * 0.27).max(10.0);
    let left = dock_left + corner_r;
    let right = dock_right - corner_r;

    let dock_mid_x = (dock_left + dock_right) / 2.0;
    let dock_mid_y = dock_bottom + dock_height / 2.0;
    let anchor_screen = find_anchor_screen(screens, dock_mid_x, dock_mid_y)?;
    let screen_left = anchor_screen.frame.x;
    let screen_right = anchor_screen.frame.x + anchor_screen.frame.width;
    if right <= left || left < screen_left || right > screen_right {
        return None;
    }

    let dock_frame = crate::cat_layout::Rect::new(
        dock_left,
        dock_bottom,
        (dock_right - dock_left).max(0.0),
        dock_height.max(0.0),
    );
    Some(DockHorizontalBounds {
        anchor_screen_id: anchor_screen.id.clone(),
        left,
        right,
        dock_frame: Some(dock_frame),
    })
}

/// 通过 Accessibility API 读取 Bottom 模式 Dock 边界 (精确方法)。
///
/// 权限状态由 `check_ax_permission` 在启动时一次性检查并缓存
#[cfg(target_os = "macos")]
fn get_dock_bounds_ax(screens: &[MacScreenSnapshot]) -> Option<DockHorizontalBounds> {
    use std::ffi::c_void;
    use std::process::Command;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};

    // 记录是否已经弹过权限申请，避免重复弹窗
    static AX_PROMPTED: AtomicBool = AtomicBool::new(false);

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
        let ax_err = AXUIElementCopyAttributeValue(dock_el, children_attr, &mut children);
        if ax_err != SUCCESS {
            CFRelease(dock_el);
            // 只在权限相关错误时弹窗：-25211 (APIDisabled) 或 -25205 (CannotComplete)
            if (ax_err == -25211 || ax_err == -25205) && !AX_PROMPTED.swap(true, Ordering::Relaxed)
            {
                use objc2_foundation::{NSDictionary, NSNumber, NSString as NSStr};
                let key = NSStr::from_str("AXTrustedCheckOptionPrompt");
                let val = NSNumber::new_bool(true);
                let dict = NSDictionary::from_id_slice(&[&*key], &[val]);
                let dict_ptr = &*dict as *const _ as CFDictionaryRef;
                AXIsProcessTrustedWithOptions(dict_ptr);
            }
            return None;
        }

        let count = CFArrayGetCount(children);
        let mut result: Option<DockHorizontalBounds> = None;

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

            result = dock_bounds_from_ax_list_metrics(
                screens,
                pos[0] as f32,
                pos[1] as f32,
                size[0] as f32,
                size[1] as f32,
            );
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
fn get_dock_bounds_estimate(screen_left: f32, screen_w: f32) -> (f32, f32) {
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

    let dock_left = screen_left + (screen_w - dock_width) / 2.0;
    let dock_right = dock_left + dock_width;

    // Dock 背景高度 ≈ tilesize + 20pt, squircle 圆角 ≈ 背景高度 × 0.27
    let dock_bg_height = tilesize + 20.0;
    let corner_r = (dock_bg_height * 0.27).max(10.0);
    let left = (dock_left + corner_r).max(screen_left);
    let right = (dock_right - corner_r).min(screen_left + screen_w);

    (left, right)
}

/// 构造屏幕稳定标识。
///
/// 语义与边界：
/// - 标识由名称与几何信息拼接而成，避免依赖额外系统字段。
/// - `visible_frame` 不参与 ID 计算，避免 Dock 自动隐藏等场景导致同一屏幕 ID 抖动。
/// - 仅用于同一次运行中的屏幕匹配，不承诺跨设备长期稳定。
#[cfg(target_os = "macos")]
fn build_screen_id(name: &str, frame: &crate::cat_layout::Rect, scale_factor: f32) -> String {
    format!(
        "{}|{:.1},{:.1},{:.1},{:.1}|{:.2}",
        name, frame.x, frame.y, frame.width, frame.height, scale_factor
    )
}

/// macOS: 通过 NSScreen 采样多屏幕快照（全局坐标）。
///
/// 语义与边界：
/// - 必须在主线程调用 AppKit 接口。
/// - 返回所有可见屏幕快照，保留 frame/visible_frame 的全局坐标。
/// - 若没有采样到屏幕，返回 `None`。
#[cfg(target_os = "macos")]
fn get_macos_screen_info() -> Option<Vec<MacScreenSnapshot>> {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSScreen;
    use objc2_foundation::{MainThreadMarker, NSString};

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let screens = NSScreen::screens(mtm);
    if screens.len() == 0 {
        return None;
    }

    let main_metrics = NSScreen::mainScreen(mtm).map(|screen| {
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        (
            frame.origin.x as f32,
            frame.origin.y as f32,
            frame.size.width as f32,
            frame.size.height as f32,
            visible.origin.x as f32,
            visible.origin.y as f32,
            visible.size.width as f32,
            visible.size.height as f32,
            screen.backingScaleFactor() as f32,
        )
    });

    let mut result = Vec::with_capacity(screens.len());
    for screen in screens.iter() {
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        let scale = screen.backingScaleFactor() as f32;

        let frame_rect = crate::cat_layout::Rect::new(
            frame.origin.x as f32,
            frame.origin.y as f32,
            frame.size.width as f32,
            frame.size.height as f32,
        );
        let visible_rect = crate::cat_layout::Rect::new(
            visible.origin.x as f32,
            visible.origin.y as f32,
            visible.size.width as f32,
            visible.size.height as f32,
        );

        let metrics = (
            frame_rect.x,
            frame_rect.y,
            frame_rect.width,
            frame_rect.height,
            visible_rect.x,
            visible_rect.y,
            visible_rect.width,
            visible_rect.height,
            scale,
        );
        let is_main = main_metrics.map_or(false, |main| main == metrics);
        let name = unsafe { screen.localizedName() }.to_string();
        let selection_id = unsafe {
            let device_description: *mut AnyObject = objc2::msg_send![&*screen, deviceDescription];
            if device_description.is_null() {
                build_screen_id(&name, &frame_rect, scale)
            } else {
                let key = NSString::from_str("NSScreenNumber");
                let screen_number: *mut AnyObject =
                    objc2::msg_send![device_description, objectForKey: &*key];
                if screen_number.is_null() {
                    build_screen_id(&name, &frame_rect, scale)
                } else {
                    let display_number: u32 = objc2::msg_send![screen_number, unsignedIntValue];
                    if display_number == 0 {
                        build_screen_id(&name, &frame_rect, scale)
                    } else {
                        format!("display-{display_number}")
                    }
                }
            }
        };

        result.push(MacScreenSnapshot {
            id: build_screen_id(&name, &frame_rect, scale),
            selection_id,
            name,
            frame: frame_rect,
            visible_frame: visible_rect,
            scale_factor: scale,
            is_main,
        });
    }

    Some(result)
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

/// macOS 透明猫窗口的外观配置。
///
/// 语义与边界：
/// - 仅描述窗口创建后的 AppKit 外观收尾，不承载 Dock 布局或渲染逻辑。
/// - `opaque=false` 与透明背景需要同时配置，避免透明窗口被系统当作不透明底色合成。
///
/// 关键副作用：
/// - 无；仅返回静态配置值。
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MacWindowAppearanceConfig {
    ignores_mouse_events: bool,
    has_shadow: bool,
    opaque: bool,
    order_front_regardless: bool,
}

/// 返回 macOS 透明猫窗口的默认外观配置。
///
/// 语义与边界：
/// - 默认保持鼠标穿透开启，后续仍由 `update_mouse_passthrough` 动态切换。
/// - 强制 `opaque=false`、透明背景、无阴影，并把窗口提升到前台可见顺序。
///
/// 返回：
/// - 透明猫窗口初始化所需的 AppKit 配置。
///
/// 错误处理与失败场景：
/// - 不返回错误；调用方负责处理窗口句柄缺失。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn macos_window_appearance_config() -> MacWindowAppearanceConfig {
    MacWindowAppearanceConfig {
        ignores_mouse_events: true,
        has_shadow: false,
        opaque: false,
        order_front_regardless: true,
    }
}

/// 返回当前应用的第一个窗口句柄。
///
/// 语义与边界：
/// - 不依赖 `mainWindow` / `keyWindow`，因为 Accessory 应用在未激活时这两个值可能为空。
/// - 仅返回窗口数组中的第一个窗口，符合当前单窗口猫应用的结构假设。
///
/// 返回：
/// - `Some(window)`：找到窗口。
/// - `None`：当前应用尚未创建任何窗口。
///
/// 错误处理与失败场景：
/// - 不返回错误；窗口数组为空时返回 `None`。
///
/// 关键副作用：
/// - 无。
#[cfg(target_os = "macos")]
fn first_app_window() -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let app = NSApplication::sharedApplication(mtm);
    unsafe {
        let windows: *mut AnyObject = objc2::msg_send![&*app, windows];
        let count: usize = objc2::msg_send![windows, count];
        if count == 0 {
            return None;
        }
        let window: *mut AnyObject = objc2::msg_send![windows, objectAtIndex: 0_usize];
        if window.is_null() {
            None
        } else {
            Some(window)
        }
    }
}

/// macOS: 初始化窗口外观（去阴影 + 默认鼠标穿透）
#[cfg(target_os = "macos")]
fn setup_window_appearance() {
    use objc2::runtime::AnyClass;
    use objc2::runtime::AnyObject;

    if let Some(window) = first_app_window() {
        let config = macos_window_appearance_config();
        unsafe {
            let _: () =
                objc2::msg_send![window, setIgnoresMouseEvents: config.ignores_mouse_events];
            let _: () = objc2::msg_send![window, setHasShadow: config.has_shadow];
            let _: () = objc2::msg_send![window, setOpaque: config.opaque];

            let ns_color_cls = AnyClass::get("NSColor").expect("NSColor class should exist");
            let clear_color: *mut AnyObject = objc2::msg_send![ns_color_cls, clearColor];
            let _: () = objc2::msg_send![window, setBackgroundColor: clear_color];

            if config.order_front_regardless {
                let _: () = objc2::msg_send![window, orderFrontRegardless];
            }
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
fn update_mouse_passthrough(cat_rects: &[egui::Rect], is_dragging: bool) {
    use objc2::runtime::AnyClass;
    use objc2_foundation::{CGPoint, CGRect};

    unsafe {
        let Some(window) = first_app_window() else {
            return;
        };

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
        let mouse_pos = egui::pos2(local_x, local_y);

        // 分别检查每只猫的区域，任一命中即不穿透
        let over_any_cat = cat_rects
            .iter()
            .any(|r| r.contains(mouse_pos));

        let _: () = objc2::msg_send![window, setIgnoresMouseEvents: !over_any_cat];
    }
}

#[cfg(not(target_os = "macos"))]
fn update_mouse_passthrough(_cat_rects: &[egui::Rect], _is_dragging: bool) {}

// ============================================================
// 入口函数
// ============================================================

/// 构造桌面像素猫的原生窗口选项。
///
/// 语义与边界：
/// - 统一收口窗口尺寸、初始位置、透明度与置顶策略，避免 `run_cat` 内散落平台细节。
/// - 显式固定 `renderer=wgpu`，避免 macOS 透明窗口回退到 `glow/OpenGL`。
///
/// 入参：
/// - `initial_layout`：启动阶段计算出的窗口布局快照。
///
/// 返回：
/// - 可直接传给 `eframe::run_native` 的 `NativeOptions`。
///
/// 错误处理与失败场景：
/// - 不返回错误；平台不支持的细节由 `eframe/winit` 在运行时兜底。
///
/// 关键副作用：
/// - 无。
fn build_cat_native_options(initial_layout: &AppliedLayout) -> eframe::NativeOptions {
    let window_width = initial_layout.window_width;
    let window_height = CELL_SIZE as f32 * SCALE + 22.0;
    let initial_pos = [
        initial_layout.window_origin.x,
        initial_layout.window_origin.y - window_height,
    ];

    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([window_width, window_height])
            .with_position(initial_pos)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    }
}

/// 启动桌面像素猫主程序。
///
/// 语义与边界：
/// - 启动时先解析一次 Dock 布局快照，再创建透明窗口与应用状态。
/// - macOS 下优先使用多屏幕采样结果；失败时退化到兜底布局。
/// - 不负责阻塞等待子线程退出，OTel 服务线程由进程生命周期托管。
///
/// 关键副作用：
/// - 新建 Tokio Runtime 并启动 OTel HTTP 接收服务。
/// - 调用 eframe 进入 GUI 主循环，直到窗口关闭。
pub fn run_cat() {
    // 后台启动 Codex OTel 接收服务器
    std::thread::spawn(|| {
        let rt = tokio::runtime::Runtime::new().expect("Cannot create tokio runtime");
        rt.block_on(crate::server::run_server(4318));
    });

    #[cfg(target_os = "macos")]
    let initial_layout = {
        if let Some(screens) = get_macos_screen_info() {
            if let Some(fallback_screen) = main_screen_snapshot(&screens) {
                let dock_sample = get_dock_sample(&screens, fallback_screen);
                compute_applied_layout_for_dock_sample(&screens, fallback_screen, &dock_sample)
                    .unwrap_or_else(|| AppliedLayout::fallback(200.0, 1200.0, 800.0))
            } else {
                AppliedLayout::fallback(200.0, 1200.0, 800.0)
            }
        } else {
            AppliedLayout::fallback(200.0, 1200.0, 800.0)
        }
    };

    #[cfg(not(target_os = "macos"))]
    let initial_layout = AppliedLayout::fallback(200.0, 1200.0, 800.0);

    let options = build_cat_native_options(&initial_layout);

    let startup_layout = initial_layout.clone();

    eframe::run_native(
        "Desktop Cat",
        options,
        Box::new(move |cc| {
            Ok(Box::new(UnifiedCatApp::new(
                &cc.egui_ctx,
                startup_layout.clone(),
            )))
        }),
    )
    .expect("Failed to start cat window");
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use crate::tray;

    use super::{
        build_bottom_mode_sample, build_floor_mode_sample, build_screen_id,
        compute_applied_layout_for_dock_sample, dock_bounds_from_ax_list_metrics,
        dock_refresh_interval_for_tick, layout_changed, legacy_visible_bottom,
        main_screen_snapshot, refresh_interval, resolve_dock_anchor_screen,
        resolve_next_applied_layout, select_side_dock_anchor_screen,
        select_side_dock_anchor_screen_with_preference, should_run_fast_autohide_probe,
        AppliedLayout, MacScreenSnapshot,
    };

    /// 构造一个仅用于布局钳位测试的哑猫实体。
    ///
    /// 语义与边界：
    /// - 仅填充 `apply_dock_result` 所需字段（`x_offset/max_width`），其余字段使用安全的占位值。
    /// - 不用于渲染与动画推进；任何依赖 `animations` 的逻辑都不应在此测试中调用。
    fn dummy_cat(id: &str, max_width: f32, x_offset: f32) -> super::CatEntity {
        let now = Instant::now();
        super::CatEntity {
            id: id.to_string(),
            is_mini: false,
            scale: 1.0,
            animations: Vec::new(),
            state: super::AnimationState {
                current_anim: 0,
                current_frame: 0,
                last_frame_time: now,
                loop_count: 0,
                max_loops: 0,
            },
            x_offset,
            move_direction: 1.0,
            last_move_time: now,
            claude_state: super::ClaudeState::Idle,
            transition_anim: None,
            pending_state: None,
            pending_permission_count: 0,
            notification_text: None,
            notification_expire: now,
            max_width,
            max_height: 0.0,
            min_bottom_offset: 0.0,
            returning: false,
            spawn_time: now,
        }
    }

    fn make_screen(
        name: &str,
        frame: crate::cat_layout::Rect,
        visible: crate::cat_layout::Rect,
        scale_factor: f32,
        is_main: bool,
    ) -> MacScreenSnapshot {
        MacScreenSnapshot {
            id: build_screen_id(name, &frame, scale_factor),
            selection_id: format!("selection-{}", build_screen_id(name, &frame, scale_factor)),
            name: name.to_string(),
            frame,
            visible_frame: visible,
            scale_factor,
            is_main,
        }
    }

    fn make_layout(
        anchor_screen_id: &str,
        window_origin_x: f32,
        base_y: f32,
        width: f32,
        mode: crate::cat_layout::DockPlacementMode,
    ) -> AppliedLayout {
        AppliedLayout {
            anchor_screen_id: anchor_screen_id.to_string(),
            window_origin: eframe::egui::pos2(window_origin_x, base_y),
            walk_bounds: crate::cat_layout::Rect::new(window_origin_x, 0.0, width, 64.0),
            dock_mode: mode,
            dock_autohide: false,
            window_width: width,
        }
    }

    #[test]
    fn layout_changed_detects_anchor_screen_change_even_when_width_unchanged() {
        let previous = make_layout(
            "main",
            0.0,
            1092.0,
            1200.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );
        let next = make_layout(
            "external",
            1728.0,
            1055.0,
            1200.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );

        assert!(layout_changed(Some(&previous), &next));
    }

    #[test]
    fn applied_layout_should_not_advance_when_layout_is_unchanged() {
        let applied = make_layout(
            "main",
            0.0,
            1092.0,
            1200.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );
        let sampled = make_layout(
            "main",
            0.2,
            1092.3,
            1200.2,
            crate::cat_layout::DockPlacementMode::Bottom,
        );

        assert!(!layout_changed(Some(&applied), &sampled));
        assert!(resolve_next_applied_layout(Some(&applied), &sampled).is_none());
    }

    #[test]
    fn build_screen_id_stays_stable_when_visible_frame_changes() {
        let frame = crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0);
        let visible_a = crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0);
        let visible_b = crate::cat_layout::Rect::new(0.0, 55.0, 1728.0, 1040.0);

        let screen_a = make_screen("Built-in Retina", frame.clone(), visible_a, 2.0, true);
        let screen_b = make_screen("Built-in Retina", frame, visible_b, 2.0, true);
        assert_eq!(screen_a.id, screen_b.id);
    }

    #[test]
    fn dock_anchor_base_y_should_match_anchor_screen() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
            true,
        );
        let external = make_screen(
            "External",
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            crate::cat_layout::Rect::new(1728.0, 40.0, 1920.0, 1040.0),
            1.0,
            false,
        );
        let screens = vec![main, external.clone()];

        let fallback_main = main_screen_snapshot(&screens).expect("main screen should exist");
        let dock_sample = build_floor_mode_sample(&external, false);
        let dock_screen = resolve_dock_anchor_screen(&screens, fallback_main, &dock_sample);

        assert_eq!(
            legacy_visible_bottom(&screens, &external),
            legacy_visible_bottom(&screens, dock_screen)
        );
        assert_ne!(
            legacy_visible_bottom(&screens, fallback_main),
            legacy_visible_bottom(&screens, dock_screen)
        );
    }

    #[test]
    fn base_y_should_use_main_display_height_when_screen_is_stacked_above_main() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1512.0, 982.0),
            crate::cat_layout::Rect::new(0.0, 58.0, 1512.0, 891.0),
            2.0,
            true,
        );
        let upper = make_screen(
            "Upper External",
            crate::cat_layout::Rect::new(-153.0, 982.0, 2560.0, 1440.0),
            crate::cat_layout::Rect::new(-153.0, 982.0, 2560.0, 1440.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), upper];

        let expected_base_y = main.frame.height - main.visible_frame.y;
        assert_eq!(legacy_visible_bottom(&screens, &main), expected_base_y);
    }

    #[test]
    fn dock_ax_metrics_should_anchor_to_main_screen_in_vertical_stack() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1512.0, 982.0),
            crate::cat_layout::Rect::new(0.0, 58.0, 1512.0, 891.0),
            2.0,
            true,
        );
        let upper = make_screen(
            "Upper External",
            crate::cat_layout::Rect::new(-153.0, 982.0, 2560.0, 1440.0),
            crate::cat_layout::Rect::new(-153.0, 982.0, 2560.0, 1440.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), upper];

        let bounds = dock_bounds_from_ax_list_metrics(&screens, 295.0, 982.0, 922.0, 52.0)
            .expect("AX geometry should still resolve to a screen");

        assert_eq!(bounds.anchor_screen_id, main.id);
    }

    #[test]
    fn manual_display_mode_should_fall_back_to_auto_when_selection_disappears() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1512.0, 982.0),
            crate::cat_layout::Rect::new(0.0, 58.0, 1512.0, 891.0),
            2.0,
            true,
        );
        let screens = vec![main];

        let mode = super::resolve_display_location_mode(
            &screens,
            super::DisplayLocationMode::Specific("missing-display".to_string()),
        );

        assert_eq!(mode, super::DisplayLocationMode::Auto);
    }

    #[test]
    fn manual_display_mode_should_use_floor_layout_when_dock_is_on_other_display() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
            true,
        );
        let external = make_screen(
            "External",
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            crate::cat_layout::Rect::new(1728.0, 25.0, 1920.0, 1035.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), external.clone()];
        let auto_sample = build_bottom_mode_sample(
            &external,
            &super::DockHorizontalBounds {
                anchor_screen_id: external.id.clone(),
                left: 2100.0,
                right: 3000.0,
                dock_frame: Some(crate::cat_layout::Rect::new(2100.0, 980.0, 900.0, 80.0)),
            },
            false,
        );

        let sample = super::resolve_display_location_dock_sample(
            &screens,
            &auto_sample,
            super::DisplayLocationMode::Specific(main.selection_id.clone()),
        );

        assert_eq!(sample.dock_snapshot.anchor_screen_id, main.id);
        assert_eq!(
            sample.dock_snapshot.mode,
            crate::cat_layout::DockPlacementMode::Floor
        );
        assert_eq!(sample.walk_bounds, main.visible_frame);
    }

    #[test]
    fn manual_display_mode_should_reuse_dock_layout_when_dock_is_on_selected_display() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
            true,
        );
        let external = make_screen(
            "External",
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            crate::cat_layout::Rect::new(1728.0, 25.0, 1920.0, 1035.0),
            1.0,
            false,
        );
        let screens = vec![main, external.clone()];
        let auto_sample = build_bottom_mode_sample(
            &external,
            &super::DockHorizontalBounds {
                anchor_screen_id: external.id.clone(),
                left: 2100.0,
                right: 3000.0,
                dock_frame: Some(crate::cat_layout::Rect::new(2100.0, 980.0, 900.0, 80.0)),
            },
            false,
        );

        let sample = super::resolve_display_location_dock_sample(
            &screens,
            &auto_sample,
            super::DisplayLocationMode::Specific(external.selection_id.clone()),
        );

        assert_eq!(sample.dock_snapshot.anchor_screen_id, external.id);
        assert_eq!(sample.dock_snapshot.mode, auto_sample.dock_snapshot.mode);
        assert_eq!(sample.walk_bounds, auto_sample.walk_bounds);
    }

    #[test]
    fn display_menu_choices_should_suffix_duplicate_names() {
        let left = make_screen(
            "Studio Display",
            crate::cat_layout::Rect::new(-1440.0, 0.0, 1440.0, 900.0),
            crate::cat_layout::Rect::new(-1440.0, 25.0, 1440.0, 850.0),
            1.0,
            false,
        );
        let right = make_screen(
            "Studio Display",
            crate::cat_layout::Rect::new(0.0, 0.0, 1440.0, 900.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1440.0, 850.0),
            1.0,
            true,
        );

        let choices = super::display_choices_from_screens(&[left, right]);

        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label, "Studio Display #1");
        assert_eq!(choices[1].label, "Studio Display #2");
    }

    #[test]
    fn applied_layout_bottom_mode_prefers_visible_frame_when_it_reserves_more_space_than_ax_frame()
    {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1512.0, 982.0),
            crate::cat_layout::Rect::new(0.0, 58.0, 1512.0, 891.0),
            2.0,
            true,
        );
        let screens = vec![main.clone()];
        let fallback_main = main_screen_snapshot(&screens).expect("main screen should exist");
        let bounds = super::DockHorizontalBounds {
            anchor_screen_id: main.id.clone(),
            left: 295.0,
            right: 1217.0,
            dock_frame: Some(crate::cat_layout::Rect::new(295.0, 52.0, 922.0, 52.0)),
        };
        let dock_sample = build_bottom_mode_sample(&main, &bounds, false);

        let layout = compute_applied_layout_for_dock_sample(&screens, fallback_main, &dock_sample)
            .expect("bottom dock layout should be computed");

        assert_eq!(layout.window_origin.y, 924.0);
    }

    #[test]
    fn applied_layout_bottom_mode_uses_dock_frame_when_upper_screen_visible_frame_loses_bottom_inset(
    ) {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1512.0, 982.0),
            crate::cat_layout::Rect::new(0.0, 58.0, 1512.0, 891.0),
            2.0,
            true,
        );
        let upper = make_screen(
            "Upper External",
            crate::cat_layout::Rect::new(-153.0, 982.0, 2560.0, 1440.0),
            crate::cat_layout::Rect::new(-153.0, 982.0, 2560.0, 1440.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), upper.clone()];
        let fallback_main = main_screen_snapshot(&screens).expect("main screen should exist");
        let bounds = super::DockHorizontalBounds {
            anchor_screen_id: upper.id.clone(),
            left: 295.0,
            right: 1217.0,
            dock_frame: Some(crate::cat_layout::Rect::new(295.0, 1034.0, 922.0, 52.0)),
        };
        let dock_sample = build_bottom_mode_sample(&upper, &bounds, true);

        let layout = compute_applied_layout_for_dock_sample(&screens, fallback_main, &dock_sample)
            .expect("upper-screen bottom dock layout should be computed");

        assert_eq!(layout.anchor_screen_id, upper.id);
        assert_eq!(layout.window_origin.y, -52.0);
    }

    #[test]
    fn base_y_uses_virtual_desktop_top_when_anchor_screen_is_below_main() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
            true,
        );
        let lower = make_screen(
            "Lower External",
            crate::cat_layout::Rect::new(0.0, -900.0, 1920.0, 900.0),
            crate::cat_layout::Rect::new(0.0, -860.0, 1920.0, 860.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), lower.clone()];

        let expected_base_y = (main.frame.y + main.frame.height) - lower.visible_frame.y;
        assert_eq!(legacy_visible_bottom(&screens, &lower), expected_base_y);
    }

    #[test]
    fn floor_mode_bounds_should_use_visible_frame_width() {
        let screen = make_screen(
            "Right Dock Screen",
            crate::cat_layout::Rect::new(0.0, 0.0, 1512.0, 982.0),
            crate::cat_layout::Rect::new(96.0, 25.0, 1416.0, 957.0),
            2.0,
            true,
        );

        let sample = build_floor_mode_sample(&screen, true);

        assert_eq!(sample.walk_bounds.x, screen.visible_frame.x);
        assert_eq!(sample.walk_bounds.width, screen.visible_frame.width);
    }

    #[test]
    fn side_dock_left_orientation_should_anchor_to_screen_with_left_inset() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
            true,
        );
        let secondary = make_screen(
            "External Left Dock",
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            crate::cat_layout::Rect::new(1808.0, 25.0, 1840.0, 1035.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), secondary.clone()];

        let dock_screen = select_side_dock_anchor_screen(&screens, &main, "left");
        assert_eq!(dock_screen.id, secondary.id);
    }

    #[test]
    fn side_dock_right_orientation_should_anchor_to_screen_with_right_inset() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
            true,
        );
        let secondary = make_screen(
            "External Right Dock",
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            crate::cat_layout::Rect::new(1728.0, 25.0, 1842.0, 1035.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), secondary.clone()];

        let dock_screen = select_side_dock_anchor_screen(&screens, &main, "right");
        assert_eq!(dock_screen.id, secondary.id);
    }

    #[test]
    fn side_dock_autohide_hidden_should_keep_preferred_anchor_screen_when_no_inset() {
        let main = make_screen(
            "Built-in Retina",
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            crate::cat_layout::Rect::new(0.0, 0.0, 1728.0, 1117.0),
            2.0,
            true,
        );
        let secondary = make_screen(
            "External Side Dock",
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            crate::cat_layout::Rect::new(1728.0, 0.0, 1920.0, 1080.0),
            1.0,
            false,
        );
        let screens = vec![main.clone(), secondary.clone()];

        let dock_screen = select_side_dock_anchor_screen_with_preference(
            &screens,
            &main,
            Some(&secondary.id),
            "left",
        );
        assert_eq!(
            dock_screen.id, secondary.id,
            "Dock 隐藏态无法通过 visible_frame 内缩推断时，应优先保留上次确认的副屏锚点"
        );
    }

    #[test]
    fn autohide_layout_uses_fast_refresh_interval() {
        assert_eq!(refresh_interval(true), Duration::from_millis(250));
        assert_eq!(refresh_interval(false), Duration::from_secs(5));
    }

    #[test]
    fn live_autohide_probe_should_override_stale_applied_layout_for_refresh() {
        let stale_layout = make_layout(
            "main",
            0.0,
            1092.0,
            1200.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );

        let interval = dock_refresh_interval_for_tick(Some(&stale_layout), Some(true));
        assert_eq!(interval, Duration::from_millis(250));
    }

    #[test]
    fn fast_autohide_probe_should_stop_when_applied_layout_is_autohide() {
        let mut autohide_layout = make_layout(
            "main",
            0.0,
            1092.0,
            1200.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );
        autohide_layout.dock_autohide = true;

        assert!(!should_run_fast_autohide_probe(Some(&autohide_layout)));
        assert!(should_run_fast_autohide_probe(None));
    }

    #[test]
    fn cat_native_options_should_prefer_wgpu_renderer() {
        let layout = make_layout(
            "main",
            0.0,
            1092.0,
            1200.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );

        let options = super::build_cat_native_options(&layout);
        assert_eq!(options.renderer.to_string(), "wgpu");
    }

    #[test]
    fn macos_window_appearance_should_force_clear_non_opaque_window() {
        let config = super::macos_window_appearance_config();

        assert!(config.ignores_mouse_events);
        assert!(!config.has_shadow);
        assert!(!config.opaque);
        assert!(config.order_front_regardless);
    }

    #[test]
    fn apply_dock_result_should_clamp_cx_main_cat_x_offset() {
        let now = Instant::now();

        let initial_layout = make_layout(
            "main",
            0.0,
            1092.0,
            300.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );
        let next_layout = make_layout(
            "main",
            0.0,
            1092.0,
            10.0,
            crate::cat_layout::DockPlacementMode::Bottom,
        );

        let dock_result = Arc::new(Mutex::new(None));
        let dock_refreshing = Arc::new(Mutex::new(false));
        let dock_autohide_probe_result = Arc::new(Mutex::new(None));
        let dock_autohide_probe_refreshing = Arc::new(Mutex::new(false));

        let mut app = super::UnifiedCatApp {
            sprite_sheet: image::RgbaImage::new(1, 1),
            cx_sprite_sheet: image::RgbaImage::new(1, 1),
            main_cat: dummy_cat("main", 50.0, 100.0),
            cx_main_cat: dummy_cat("cx-main", 50.0, 100.0),
            mini_cats: vec![dummy_cat("mini", 30.0, 80.0)],
            position_phase: 10,
            applied_layout: Some(initial_layout),
            window_width: 300.0,
            window_height: 0.0,
            dock_left: 0.0,
            dock_right: 300.0,
            base_y: 1092.0,
            last_poll_time: now,
            last_dock_refresh: now,
            last_display_location_revision: 0,
            dock_autohide_probe: None,
            last_dock_autohide_probe: now,
            dock_autohide_probe_result,
            dock_autohide_probe_refreshing,
            last_event_time: None,
            known_subagents: HashSet::new(),
            debug_subagents_active: false,
            dock_result: Arc::clone(&dock_result),
            dock_refreshing,
            app_start: now,
            drag_offset: eframe::egui::Vec2::ZERO,
            is_dragging: false,
            snap_back_start: None,
            last_cat_rect: eframe::egui::Rect::NOTHING,
            cx_drag_offset: eframe::egui::Vec2::ZERO,
            cx_is_dragging: false,
            cx_snap_back_start: None,
            cx_last_cat_rect: eframe::egui::Rect::NOTHING,
            tray_anim_state: tray::TrayAnimState::new(),
            tray_last_frame_time: now,
            tray_status_item: None,
            tray_nsimages: Vec::new(),
        };

        {
            let mut lock = dock_result.lock().unwrap();
            *lock = Some(super::DockBoundsResult {
                layout: next_layout,
            });
        }

        assert!(app.apply_dock_result(), "dock layout should be applied");
        assert_eq!(
            app.cx_main_cat.x_offset, 0.0,
            "cx_main_cat should be clamped together with main_cat"
        );
    }
}
