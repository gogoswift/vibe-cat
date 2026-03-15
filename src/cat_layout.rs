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
/// - `x`/`y` 的语义由调用方坐标系定义，`width`/`height` 为非负尺寸。
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
    /// - `x`/`y`：调用方坐标系下的基准点（point）。
    /// - `width`/`height`：矩形尺寸（point），调用方应保证非负。
    ///
    /// 返回：
    /// - 新的 `Rect` 值；不做归一化与裁剪。
    ///
    /// 错误处理：
    /// - 不返回错误；非法几何由上层在布局阶段处理。
    ///
    /// 关键副作用：
    /// - 无。
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// 判断点是否落在矩形内（含边界）。
    ///
    /// 入参：
    /// - `point_x`/`point_y`：与矩形相同坐标系下的点坐标（point）。
    ///
    /// 返回：
    /// - `true`：点在矩形内部或边界上。
    /// - `false`：点在矩形外。
    ///
    /// 错误处理：
    /// - 不返回错误；若矩形尺寸非法（负值），结果由比较表达式自然决定。
    ///
    /// 关键副作用：
    /// - 无。
    pub fn contains_point(&self, point_x: f32, point_y: f32) -> bool {
        point_x >= self.x
            && point_x <= self.x + self.width
            && point_y >= self.y
            && point_y <= self.y + self.height
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
    ///
    /// 关键副作用：
    /// - 无。
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
    /// Bottom 模式可选的“活动范围”覆盖值。
    ///
    /// 语义与边界：
    /// - 仅在 `mode=Bottom` 时有意义，用于描述猫窗口真正可活动的水平范围。
    /// - 典型场景：运行时从 AX 读到的 `dock_frame` 可能比实际可活动范围更窄/更宽，
    ///   但窗口宽度需要跟随 `left/right` 估算边界，而不是 Dock 本体矩形。
    /// - `None` 表示沿用 `dock_frame` 作为活动范围（保持历史纯函数行为）。
    ///
    /// 关键副作用：
    /// - 无。
    pub walk_bounds_override: Option<Rect>,
    pub mode: DockPlacementMode,
    pub autohide: bool,
}

impl DockSnapshot {
    /// 构造底部 Dock 快照，并显式指定可活动范围覆盖值。
    ///
    /// 语义与边界：
    /// - `dock_frame` 仍表示 Dock 本体矩形，用于计算停靠基线与垂直位置。
    /// - `walk_bounds_override` 用于覆盖窗口宽度与水平活动范围（例如由 `left/right` 推断）。
    /// - 仅在 `mode=Bottom` 下生效；调用方应保证覆盖值与 `dock_frame` 同属一个坐标系。
    ///
    /// 入参：
    /// - `anchor_screen_id`：目标屏幕标识。
    /// - `dock_frame`：Dock 本体矩形（point）。
    /// - `walk_bounds_override`：活动范围覆盖矩形（point）。
    /// - `autohide`：Dock 是否自动隐藏。
    ///
    /// 返回：
    /// - `mode=Bottom` 且携带活动范围覆盖值的 `DockSnapshot`。
    ///
    /// 错误处理：
    /// - 不返回错误；调用方需保证覆盖值语义正确。
    ///
    /// 关键副作用：
    /// - 无。
    pub fn bottom_with_walk_bounds(
        anchor_screen_id: impl Into<String>,
        dock_frame: Rect,
        walk_bounds_override: Rect,
        autohide: bool,
    ) -> Self {
        Self {
            anchor_screen_id: anchor_screen_id.into(),
            dock_frame: Some(dock_frame),
            walk_bounds_override: Some(walk_bounds_override),
            mode: DockPlacementMode::Bottom,
            autohide,
        }
    }

    /// 构造侧边 Dock 退化模式快照。
    ///
    /// 入参：
    /// - `anchor_screen_id`：目标屏幕标识。
    /// - `autohide`：Dock 是否自动隐藏。
    ///
    /// 返回：
    /// - 固定 `mode=Floor` 且 `dock_frame=None` 的 `DockSnapshot`。
    ///
    /// 错误处理：
    /// - 不返回错误；构造函数内部保证模式合法，避免生成无效状态。
    ///
    /// 关键副作用：
    /// - 无。
    pub fn side(anchor_screen_id: impl Into<String>, autohide: bool) -> Self {
        Self {
            anchor_screen_id: anchor_screen_id.into(),
            dock_frame: None,
            walk_bounds_override: None,
            mode: DockPlacementMode::Floor,
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
            let walk_bounds = dock
                .walk_bounds_override
                .as_ref()
                .unwrap_or(dock_frame);
            Some(CatWindowLayout {
                anchor_screen_id: screen.id.clone(),
                window_origin: egui::pos2(
                    walk_bounds.x,
                    dock_frame.y - cat_height - bubble_padding,
                ),
                walk_bounds: walk_bounds.clone(),
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
        let dock_frame = Rect::new(2140.0, 980.0, 900.0, 80.0);
        let dock = DockSnapshot::bottom_with_walk_bounds(
            "external",
            dock_frame.clone(),
            dock_frame,
            false,
        );

        let layout = compute_cat_window_layout(&screens, &dock, 96.0, 22.0).unwrap();

        assert_eq!(layout.anchor_screen_id, "external");
        assert_eq!(layout.window_origin.x, 2140.0);
        assert!(layout.window_origin.y < 980.0);
    }

    #[test]
    fn bottom_dock_uses_walk_bounds_override_for_window_width_and_origin_x() {
        let screens = vec![ScreenSnapshot::new(
            "primary",
            Rect::new(0.0, 0.0, 1728.0, 1117.0),
            Rect::new(0.0, 25.0, 1728.0, 1070.0),
            2.0,
        )];

        let dock_frame = Rect::new(200.0, 980.0, 400.0, 80.0);
        let walk_bounds = Rect::new(0.0, 980.0, 1728.0, 80.0);
        let dock = DockSnapshot::bottom_with_walk_bounds("primary", dock_frame, walk_bounds, false);

        let layout = compute_cat_window_layout(&screens, &dock, 96.0, 22.0).unwrap();

        assert_eq!(layout.walk_bounds.x, 0.0);
        assert_eq!(layout.walk_bounds.width, 1728.0);
        assert_eq!(layout.window_origin.x, 0.0);
    }

    #[test]
    fn side_dock_falls_back_to_floor_mode_inside_visible_frame() {
        let screens = vec![ScreenSnapshot::new(
            "primary",
            Rect::new(0.0, 0.0, 1512.0, 982.0),
            Rect::new(96.0, 25.0, 1416.0, 957.0),
            2.0,
        )];
        let dock = DockSnapshot::side("primary", true);

        let layout = compute_cat_window_layout(&screens, &dock, 96.0, 22.0).unwrap();

        assert_eq!(layout.window_origin.x, 96.0);
        assert_eq!(layout.window_origin.y, 864.0);
        assert_eq!(layout.walk_bounds.width, 1416.0);
        assert_eq!(layout.mode, DockPlacementMode::Floor);
    }

    #[test]
    fn bottom_dock_on_left_side_monitor_accepts_negative_global_x() {
        let screens = vec![
            ScreenSnapshot::new(
                "left",
                Rect::new(-1440.0, 0.0, 1440.0, 900.0),
                Rect::new(-1440.0, 25.0, 1440.0, 850.0),
                1.0,
            ),
            ScreenSnapshot::new(
                "main",
                Rect::new(0.0, 0.0, 1728.0, 1117.0),
                Rect::new(0.0, 25.0, 1728.0, 1070.0),
                2.0,
            ),
        ];
        let dock_frame = Rect::new(-1200.0, 820.0, 800.0, 64.0);
        let dock = DockSnapshot::bottom_with_walk_bounds("left", dock_frame.clone(), dock_frame, false);

        let layout = compute_cat_window_layout(&screens, &dock, 96.0, 22.0).unwrap();

        assert!(layout.window_origin.x < 0.0);
        assert!(screens[0]
            .frame
            .contains_point(layout.window_origin.x, layout.window_origin.y));
    }
}
