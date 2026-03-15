//! Dock 布局纯计算模块。
//!
//! 职责与边界：
//! - 负责把屏幕快照与 Dock 快照转换为猫窗口的布局结果。
//! - 仅提供与平台无关的纯计算逻辑，不访问 macOS API。
//! - 不处理窗口绘制、线程调度和事件采集。
//!
//! 关键副作用：
//! - 无外部 I/O 和全局状态修改；函数应保持纯函数特性。
//!
//! 关键依赖与约束：
//! - 依赖调用方提供完整几何输入（单位统一为 point）。
//! - 输入缺失或不一致时返回 `None`，不在本模块内兜底推断。

use eframe::egui;

/// 二维矩形（point 坐标系）。
///
/// 语义与边界：
/// - `x`/`y` 为左上角原点，`width`/`height` 为非负尺寸。
/// - 本结构只承载几何数据，不负责坐标系转换。
///
/// 关键副作用：
/// - 无。
#[derive(Clone, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// 构造一个矩形。
    ///
    /// 入参：
    /// - `x`/`y`：左上角坐标（point）。
    /// - `width`/`height`：矩形尺寸（point），调用方应保证非负。
    ///
    /// 返回：
    /// - 新的 `Rect` 值；不做归一化与裁剪。
    ///
    /// 错误处理：
    /// - 不返回错误；非法几何由上层在布局阶段处理。
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// 单块屏幕的布局快照。
///
/// 语义与边界：
/// - `id` 由调用方提供并保证唯一，用于跨刷新匹配同一屏幕。
/// - `frame` 为整块屏幕区域，`visible_frame` 为可见区域。
/// - `scale_factor` 仅透传，不在本任务内参与缩放换算。
///
/// 关键副作用：
/// - 无。
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenSnapshot {
    pub id: String,
    pub frame: Rect,
    pub visible_frame: Rect,
    pub scale_factor: f32,
}

impl ScreenSnapshot {
    /// 构造屏幕快照。
    ///
    /// 入参：
    /// - `id`：屏幕标识；应在一次计算输入中唯一。
    /// - `frame`：屏幕完整区域。
    /// - `visible_frame`：扣除系统占用后的可见区域。
    /// - `scale_factor`：屏幕缩放倍率。
    ///
    /// 返回：
    /// - 可用于布局计算的 `ScreenSnapshot`。
    ///
    /// 错误处理：
    /// - 不返回错误；冲突或缺失由 `compute_cat_window_layout` 处理。
    pub fn new(id: impl Into<String>, frame: Rect, visible_frame: Rect, scale_factor: f32) -> Self {
        Self {
            id: id.into(),
            frame,
            visible_frame,
            scale_factor,
        }
    }
}

/// Dock 对应的布局模式。
///
/// 语义与边界：
/// - `Bottom`：按 Dock 实际矩形贴边。
/// - `Floor`：侧边 Dock 退化模式，沿 `visible_frame` 底部活动。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockPlacementMode {
    Bottom,
    Floor,
}

/// Dock 快照输入。
///
/// 语义与边界：
/// - `anchor_screen_id` 指定本次布局要锚定的屏幕。
/// - `dock_frame` 仅在 `Bottom` 模式需要；`Floor` 模式可为空。
/// - `autohide` 在本任务仅透传，不改变布局公式。
///
/// 关键副作用：
/// - 无。
#[derive(Clone, Debug, PartialEq)]
pub struct DockSnapshot {
    pub anchor_screen_id: String,
    pub dock_frame: Option<Rect>,
    pub mode: DockPlacementMode,
    pub autohide: bool,
}

impl DockSnapshot {
    /// 构造底部 Dock 快照。
    ///
    /// 入参：
    /// - `anchor_screen_id`：目标屏幕标识。
    /// - `dock_frame`：Dock 矩形（point）。
    /// - `autohide`：Dock 是否自动隐藏。
    ///
    /// 返回：
    /// - `mode=Bottom` 的 `DockSnapshot`。
    pub fn bottom(anchor_screen_id: impl Into<String>, dock_frame: Rect, autohide: bool) -> Self {
        Self {
            anchor_screen_id: anchor_screen_id.into(),
            dock_frame: Some(dock_frame),
            mode: DockPlacementMode::Bottom,
            autohide,
        }
    }

    /// 构造侧边 Dock 退化模式快照。
    ///
    /// 入参：
    /// - `anchor_screen_id`：目标屏幕标识。
    /// - `mode`：本任务只接受 `DockPlacementMode::Floor`。
    /// - `autohide`：Dock 是否自动隐藏。
    ///
    /// 返回：
    /// - `DockSnapshot`；调用方需保证传入模式符合约定。
    pub fn side(anchor_screen_id: impl Into<String>, mode: DockPlacementMode, autohide: bool) -> Self {
        Self {
            anchor_screen_id: anchor_screen_id.into(),
            dock_frame: None,
            mode,
            autohide,
        }
    }
}

/// 猫窗口的布局输出。
///
/// 语义与边界：
/// - `window_origin` 为窗口左上角的全局坐标。
/// - `walk_bounds` 为猫可移动区域（保持与输入同一坐标系）。
/// - `mode` 透传本次布局所使用的 Dock 规则。
#[derive(Clone, Debug, PartialEq)]
pub struct CatWindowLayout {
    pub anchor_screen_id: String,
    pub window_origin: egui::Pos2,
    pub walk_bounds: Rect,
    pub mode: DockPlacementMode,
}

/// 计算猫窗口布局。
///
/// 语义与边界：
/// - 仅基于传入快照做纯计算，不访问平台 API。
/// - `Bottom` 模式使用 `dock_frame` 作为移动边界与停靠基线。
/// - `Floor` 模式使用目标屏幕 `visible_frame` 的底边作为基线。
///
/// 入参：
/// - `screens`：可选屏幕列表，必须包含 `dock.anchor_screen_id`。
/// - `dock`：Dock 快照与布局模式。
/// - `cat_height`：猫窗口高度（point）。
/// - `bubble_padding`：猫底部留白（point）。
///
/// 返回：
/// - `Some(CatWindowLayout)`：输入完整且可计算。
/// - `None`：找不到锚定屏幕，或 `Bottom` 模式缺失 `dock_frame`。
///
/// 错误处理：
/// - 使用 `Option` 表示可恢复输入错误，不抛异常。
///
/// 关键副作用：
/// - 无。
pub fn compute_cat_window_layout(
    screens: &[ScreenSnapshot],
    dock: &DockSnapshot,
    cat_height: f32,
    bubble_padding: f32,
) -> Option<CatWindowLayout> {
    let screen = screens
        .iter()
        .find(|candidate| candidate.id == dock.anchor_screen_id)?;

    match dock.mode {
        DockPlacementMode::Bottom => {
            let dock_frame = dock.dock_frame.as_ref()?;
            Some(CatWindowLayout {
                anchor_screen_id: screen.id.clone(),
                window_origin: egui::pos2(dock_frame.x, dock_frame.y - cat_height - bubble_padding),
                walk_bounds: dock_frame.clone(),
                mode: DockPlacementMode::Bottom,
            })
        }
        DockPlacementMode::Floor => {
            let visible_frame = &screen.visible_frame;
            Some(CatWindowLayout {
                anchor_screen_id: screen.id.clone(),
                window_origin: egui::pos2(
                    visible_frame.x,
                    visible_frame.y + visible_frame.height - cat_height - bubble_padding,
                ),
                walk_bounds: visible_frame.clone(),
                mode: DockPlacementMode::Floor,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        compute_cat_window_layout, DockPlacementMode, DockSnapshot, Rect, ScreenSnapshot,
    };

    #[test]
    fn bottom_dock_on_secondary_screen_keeps_global_origin() {
        let screens = vec![
            ScreenSnapshot::new(
                "primary",
                Rect::new(0.0, 0.0, 1728.0, 1117.0),
                Rect::new(0.0, 25.0, 1728.0, 1070.0),
                2.0,
            ),
            ScreenSnapshot::new(
                "external",
                Rect::new(1728.0, 0.0, 1920.0, 1080.0),
                Rect::new(1728.0, 25.0, 1920.0, 1035.0),
                1.0,
            ),
        ];
        let dock = DockSnapshot::bottom("external", Rect::new(2140.0, 980.0, 900.0, 80.0), false);

        let layout = compute_cat_window_layout(&screens, &dock, 96.0, 22.0).unwrap();

        assert_eq!(layout.anchor_screen_id, "external");
        assert_eq!(layout.window_origin.x, 2140.0);
        assert!(layout.window_origin.y < 980.0);
    }

    #[test]
    fn side_dock_falls_back_to_floor_mode_inside_visible_frame() {
        let screens = vec![ScreenSnapshot::new(
            "primary",
            Rect::new(0.0, 0.0, 1512.0, 982.0),
            Rect::new(96.0, 25.0, 1416.0, 957.0),
            2.0,
        )];
        let dock = DockSnapshot::side("primary", DockPlacementMode::Floor, true);

        let layout = compute_cat_window_layout(&screens, &dock, 96.0, 22.0).unwrap();

        assert_eq!(layout.window_origin.x, 96.0);
        assert_eq!(layout.walk_bounds.width, 1416.0);
    }
}
