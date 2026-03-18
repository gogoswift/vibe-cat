//! macOS 状态栏托盘图标与菜单。
//!
//! 职责与边界：
//! - 负责从精灵图预创建托盘动画帧，并在运行时更新状态栏图标。
//! - 负责处理托盘左键/右键交互、菜单勾选状态与少量菜单文案刷新。
//! - 不负责窗口业务状态计算；具体猫状态由 `crate::cat` 驱动。
//!
//! 关键副作用：
//! - 读写 macOS `NSStatusItem`、`NSMenu`、`NSMenuItem` 等 Cocoa 对象。
//! - 左键会切换应用窗口可见性，右键会弹出托盘菜单。
//! - 菜单弹出前会读取当前国际化语言并刷新受支持菜单项标题。
//!
//! 关键依赖与约束：
//! - 仅在 macOS 生效，依赖 `objc2` / `objc2-app-kit` 与主线程 UI 约束。
//! - 托盘菜单对象在应用生命周期内常驻，因此通过全局指针缓存少量菜单项引用。

#![cfg(target_os = "macos")]

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use image::{DynamicImage, GenericImageView, ImageEncoder, RgbaImage};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::sel;
use objc2::ClassType;
use objc2_app_kit::{NSApplication, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
use objc2_foundation::{MainThreadMarker, NSData, NSSize, NSString};

use crate::cat::{ClaudeState, DisplayChoice, DisplayLocationMode};
use crate::i18n::{self, TranslationKey};

// ── 常量 ──

const FRAME_SIZE: u32 = 32;
const PIXEL_SCALE: u32 = 4;
const CANVAS_W: u32 = 17; // max(所有动画宽度)
const CANVAS_H: u32 = 11; // 统一画布高度

/// 托盘动画裁剪定义（精灵图中各动画的有效像素区域）
struct TrayAnimDef {
    #[allow(dead_code)]
    name: &'static str,
    row: u32,
    frames: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

/// 裁剪参数与参考项目完全一致
const TRAY_ANIMS: &[TrayAnimDef] = &[
    TrayAnimDef {
        name: "sit1",
        row: 0,
        frames: 4,
        x: 9,
        y: 20,
        w: 12,
        h: 12,
    },
    TrayAnimDef {
        name: "sit2",
        row: 1,
        frames: 4,
        x: 9,
        y: 20,
        w: 12,
        h: 12,
    },
    TrayAnimDef {
        name: "sit3",
        row: 2,
        frames: 4,
        x: 9,
        y: 20,
        w: 13,
        h: 12,
    },
    TrayAnimDef {
        name: "sit4",
        row: 3,
        frames: 4,
        x: 9,
        y: 20,
        w: 13,
        h: 12,
    },
    TrayAnimDef {
        name: "run1",
        row: 4,
        frames: 8,
        x: 7,
        y: 20,
        w: 17,
        h: 12,
    },
    TrayAnimDef {
        name: "run2",
        row: 5,
        frames: 8,
        x: 7,
        y: 19,
        w: 17,
        h: 13,
    },
    TrayAnimDef {
        name: "sleep",
        row: 6,
        frames: 4,
        x: 8,
        y: 24,
        w: 16,
        h: 8,
    },
    TrayAnimDef {
        name: "play",
        row: 7,
        frames: 6,
        x: 10,
        y: 20,
        w: 15,
        h: 12,
    },
    TrayAnimDef {
        name: "pounce",
        row: 8,
        frames: 7,
        x: 9,
        y: 14,
        w: 15,
        h: 18,
    },
    TrayAnimDef {
        name: "stretch",
        row: 9,
        frames: 8,
        x: 7,
        y: 21,
        w: 17,
        h: 11,
    },
];

const ANIM_SLEEP: usize = 6;
const ANIM_STRETCH: usize = 9;

/// Active 状态的加权动画池
struct ActiveAnimEntry {
    index: usize,
    weight: u32,
    duration_ms: u64,
}

const ACTIVE_ANIMS: &[ActiveAnimEntry] = &[
    ActiveAnimEntry {
        index: 0,
        weight: 1,
        duration_ms: 15000,
    },
    ActiveAnimEntry {
        index: 1,
        weight: 1,
        duration_ms: 15000,
    },
    ActiveAnimEntry {
        index: 2,
        weight: 1,
        duration_ms: 15000,
    },
    ActiveAnimEntry {
        index: 3,
        weight: 1,
        duration_ms: 15000,
    },
];

// ── 全局状态（点击处理用） ──

pub use crate::cat::CAT_VISIBLE;
pub use crate::cat::CC_ENABLED;
pub use crate::cat::CX_ENABLED;

/// 右键菜单指针（main thread only，应用生命周期内有效）
static MENU_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// NSStatusItem 指针（用于 popUpStatusItemMenu 定位到状态栏正下方）
static STATUS_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// 菜单处理对象指针（用于动态菜单项 target 复用）
static MENU_HANDLER_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// cc 菜单项指针（用于更新勾选状态）
static CC_MENU_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// cx 菜单项指针（用于更新勾选状态）
static CX_MENU_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// 显示位置父菜单项指针（用于动态显隐与标题刷新）
static DISPLAY_LOCATION_MENU_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// 显示位置子菜单指针（用于弹出前动态重建）
static DISPLAY_LOCATION_SUBMENU_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// 事件监控菜单项指针（用于弹出菜单前刷新国际化标题）
static GUI_MENU_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// 退出菜单项指针（用于弹出菜单前刷新国际化标题）
static QUIT_MENU_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// 本次运行是否曾检测到多显示器。
static DISPLAY_LOCATION_MENU_ACTIVATED: AtomicBool = AtomicBool::new(false);
/// 显示位置运行时状态版本号，用于通知猫窗口立即刷新布局。
static DISPLAY_LOCATION_REVISION: AtomicU64 = AtomicU64::new(0);
/// 显示位置运行时状态。
static DISPLAY_LOCATION_MODE: OnceLock<Mutex<DisplayLocationMode>> = OnceLock::new();

/// 返回显示位置运行时状态的共享存储。
///
/// 语义与边界：
/// - 仅在当前进程生命周期内保存，不做持久化。
/// - 默认值固定为 `DisplayLocationMode::Auto`。
///
/// 返回值：
/// - 全局唯一的运行时状态互斥锁。
///
/// 错误处理：
/// - 不返回错误；初始化失败将遵循 `OnceLock` 的 panic 语义。
fn display_location_mode_cell() -> &'static Mutex<DisplayLocationMode> {
    DISPLAY_LOCATION_MODE.get_or_init(|| Mutex::new(DisplayLocationMode::Auto))
}

/// 读取当前生效的显示位置模式。
///
/// 语义与边界：
/// - 返回值仅反映本次运行内的临时选择。
/// - 不会访问 AppKit，也不依赖当前显示器采样结果。
///
/// 返回值：
/// - 当前保存的 `DisplayLocationMode`。
///
/// 错误处理：
/// - 共享状态加锁失败时保持当前实现风格并 panic。
pub(crate) fn current_display_location_mode() -> DisplayLocationMode {
    display_location_mode_cell().lock().unwrap().clone()
}

/// 更新当前显示位置模式，并在变更时递增刷新版本号。
///
/// 语义与边界：
/// - 仅在新旧值不同的时候才递增版本号，避免无意义重摆。
/// - 不负责校验目标显示器当前是否存在；该校验由采样路径完成。
///
/// 入参：
/// - `next_mode`：新的显示位置模式。
///
/// 返回值：
/// - 无。
///
/// 错误处理：
/// - 共享状态加锁失败时保持当前实现风格并 panic。
///
/// 关键副作用：
/// - 模式变化时会递增 `DISPLAY_LOCATION_REVISION`，通知猫窗口尽快刷新布局。
pub(crate) fn set_display_location_mode(next_mode: DisplayLocationMode) {
    let mut mode = display_location_mode_cell().lock().unwrap();
    if *mode != next_mode {
        *mode = next_mode;
        DISPLAY_LOCATION_REVISION.fetch_add(1, Ordering::Relaxed);
    }
}

/// 返回当前显示位置状态的刷新版本号。
///
/// 语义与边界：
/// - 仅用于窗口刷新节流与变化检测。
/// - 值只在本次运行内单调递增，不做持久化。
///
/// 返回值：
/// - 当前显示位置状态版本号。
pub(crate) fn display_location_revision() -> u64 {
    DISPLAY_LOCATION_REVISION.load(Ordering::Relaxed)
}

// ── 三态状态机 ──

#[derive(Clone, Copy, PartialEq)]
pub enum TrayState {
    Sleeping,
    Waking,
    Active,
}

pub struct TrayAnimState {
    pub state: TrayState,
    pub current_anim: usize,
    pub frame_index: u32,
    pub stretch_count: u32,
    pub anim_start_ms: u64,
    pub current_anim_duration: u64,
}

impl TrayAnimState {
    pub fn new() -> Self {
        Self {
            state: TrayState::Sleeping,
            current_anim: ANIM_SLEEP,
            frame_index: 0,
            stretch_count: 0,
            anim_start_ms: now_ms(),
            current_anim_duration: 0,
        }
    }
}

// ── 点击处理 ──

/// 注册 ObjC 点击处理类（仅执行一次）
fn register_tray_handler_class() -> &'static AnyClass {
    // 在闭包内定义 extern "C" fn 确保 HRTB 生命周期正确
    extern "C" fn tray_clicked(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let app = NSApplication::sharedApplication(mtm);

            // 获取当前事件类型
            let event: *mut AnyObject = msg_send![&*app, currentEvent];
            if event.is_null() {
                return;
            }
            let event_type: isize = msg_send![event, type];

            // NSEventType: 1=leftDown, 2=leftUp, 3=rightDown, 4=rightUp
            if event_type == 3 || event_type == 4 {
                // 右键弹出菜单前更新勾选状态与国际化标题
                refresh_display_location_menu();
                update_menu_check_marks();
                update_menu_localized_titles();
                let menu_ptr = MENU_PTR.load(Ordering::Acquire);
                let item_ptr = STATUS_ITEM_PTR.load(Ordering::Acquire);
                if !menu_ptr.is_null() && !item_ptr.is_null() {
                    let _: () = msg_send![item_ptr, popUpStatusItemMenu: menu_ptr];
                }
            } else {
                // 左键：切换猫窗口可见性
                let was_visible = CAT_VISIBLE.fetch_xor(true, Ordering::Relaxed);
                let alpha: f64 = if was_visible { 0.0 } else { 1.0 };

                // 设置所有窗口的 alpha
                let windows: *mut AnyObject = msg_send![&*app, windows];
                let count: usize = msg_send![windows, count];
                for i in 0..count {
                    let window: *mut AnyObject = msg_send![windows, objectAtIndex: i];
                    let _: () = msg_send![window, setAlphaValue: alpha];
                }
            }
        }
    }

    extern "C" fn toggle_cc(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
        CC_ENABLED.fetch_xor(true, Ordering::Relaxed);
    }

    extern "C" fn toggle_cx(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
        CX_ENABLED.fetch_xor(true, Ordering::Relaxed);
    }

    extern "C" fn select_display_location_auto(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
        set_display_location_mode(DisplayLocationMode::Auto);
    }

    extern "C" fn select_display_location(_this: &AnyObject, _cmd: Sel, sender: &AnyObject) {
        unsafe {
            let represented: *mut AnyObject = msg_send![sender, representedObject];
            if represented.is_null() {
                return;
            }
            let selection_id = (&*(represented as *const NSString)).to_string();
            set_display_location_mode(DisplayLocationMode::Specific(selection_id));
        }
    }

    extern "C" fn open_gui(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
        let exe = std::env::current_exe().unwrap_or_default();
        let _ = std::process::Command::new(exe).arg("gui").spawn();
    }

    extern "C" fn quit_app(_this: &AnyObject, _cmd: Sel, _sender: &AnyObject) {
        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let app = NSApplication::sharedApplication(mtm);
            let _: () = msg_send![&*app, terminate: ptr::null::<AnyObject>()];
        }
    }

    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        let superclass = AnyClass::get("NSObject").unwrap();
        let mut builder = ClassBuilder::new("ClaudeCatTrayHandler", superclass).unwrap();
        unsafe {
            builder.add_method(sel!(trayClicked:), tray_clicked as extern "C" fn(_, _, _));
            builder.add_method(sel!(toggleCC:), toggle_cc as extern "C" fn(_, _, _));
            builder.add_method(sel!(toggleCX:), toggle_cx as extern "C" fn(_, _, _));
            builder.add_method(
                sel!(selectDisplayLocationAuto:),
                select_display_location_auto as extern "C" fn(_, _, _),
            );
            builder.add_method(
                sel!(selectDisplayLocation:),
                select_display_location as extern "C" fn(_, _, _),
            );
            builder.add_method(sel!(openGui:), open_gui as extern "C" fn(_, _, _));
            builder.add_method(sel!(quitApp:), quit_app as extern "C" fn(_, _, _));
        }
        builder.register()
    })
}

/// 弹出菜单前刷新 cc/cx 菜单项的勾选状态。
///
/// 语义与边界：
/// - 仅根据当前原子开关同步勾选状态，不负责创建菜单项或处理点击事件。
///
/// 错误处理：
/// - 若菜单项尚未初始化或指针为空，则静默跳过。
///
/// 关键副作用：
/// - 会直接写入已有 `NSMenuItem` 的勾选状态。
fn update_menu_check_marks() {
    unsafe {
        let cc_ptr = CC_MENU_ITEM_PTR.load(Ordering::Acquire);
        if !cc_ptr.is_null() {
            // NSControlStateValueOn = 1, NSControlStateValueOff = 0
            let state: isize = if CC_ENABLED.load(Ordering::Relaxed) {
                1
            } else {
                0
            };
            let _: () = msg_send![cc_ptr, setState: state];
        }
        let cx_ptr = CX_MENU_ITEM_PTR.load(Ordering::Acquire);
        if !cx_ptr.is_null() {
            let state: isize = if CX_ENABLED.load(Ordering::Relaxed) {
                1
            } else {
                0
            };
            let _: () = msg_send![cx_ptr, setState: state];
        }
    }
}

/// 返回指定托盘菜单 key 在当前生效语言下的标题。
///
/// 入参：
/// - `key`: 托盘菜单对应的稳定翻译 key。
///
/// 返回值：
/// - 当前语言下应显示的静态菜单标题。
///
/// 错误处理：
/// - 不会失败；语言解析失败时 `crate::i18n` 会自动回退英文。
fn localized_menu_title(key: TranslationKey) -> &'static str {
    i18n::translate(i18n::current_language(), key)
}

/// 为已创建的托盘菜单项刷新国际化标题。
///
/// 语义与边界：
/// - 仅更新当前已经缓存了指针的菜单项标题，不重建菜单结构。
/// - 该函数用于为未来“手动切换语言”预留刷新能力。
///
/// 错误处理：
/// - 如果菜单项尚未初始化、指针为空或标题对象无法创建，则静默跳过当前项。
///
/// 关键副作用：
/// - 会直接修改 Cocoa 菜单项标题，并影响下一次托盘菜单显示内容。
fn update_menu_localized_titles() {
    unsafe {
        let display_location_ptr = DISPLAY_LOCATION_MENU_ITEM_PTR.load(Ordering::Acquire);
        if !display_location_ptr.is_null() {
            let title = NSString::from_str(localized_menu_title(TranslationKey::DisplayLocation));
            let _: () = msg_send![display_location_ptr, setTitle: &*title];
        }

        let gui_ptr = GUI_MENU_ITEM_PTR.load(Ordering::Acquire);
        if !gui_ptr.is_null() {
            let title = NSString::from_str(localized_menu_title(TranslationKey::EventMonitor));
            let _: () = msg_send![gui_ptr, setTitle: &*title];
        }

        let quit_ptr = QUIT_MENU_ITEM_PTR.load(Ordering::Acquire);
        if !quit_ptr.is_null() {
            let title = NSString::from_str(localized_menu_title(TranslationKey::Quit));
            let _: () = msg_send![quit_ptr, setTitle: &*title];
        }
    }
}

/// 判断“显示位置”菜单在本次运行中是否应该可见。
///
/// 语义与边界：
/// - 当前检测到多个显示器时立即可见。
/// - 只要本次运行中曾出现过多显示器，之后即使退回单显示器也保持可见。
///
/// 入参：
/// - `has_seen_multiple_displays`: 本次运行是否曾经检测到多个显示器。
/// - `display_count`: 当前检测到的显示器数量。
///
/// 返回值：
/// - `true` 表示应显示“显示位置”菜单。
/// - `false` 表示应隐藏该菜单。
///
/// 错误处理：
/// - 不返回错误；调用方需保证 `display_count` 来自同一轮采样。
fn should_show_display_location_menu(
    has_seen_multiple_displays: bool,
    display_count: usize,
) -> bool {
    has_seen_multiple_displays || display_count > 1
}

/// 托盘“工具类”菜单项的稳定顺序键。
///
/// 语义与边界：
/// - 只描述显示位置与事件监控这组菜单项的相对顺序。
/// - 不承载分隔线、退出项或 agent 开关项。
///
/// 关键副作用：
/// - 无。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrayUtilityMenuItem {
    DisplayLocation,
    EventMonitor,
}

/// 返回托盘工具菜单组的稳定顺序。
///
/// 语义与边界：
/// - 用于统一约束“显示位置”和“事件监控”的相对顺序。
/// - 当前约定为先显示“显示位置”，再显示“事件监控”，二者同属一个分组。
///
/// 返回值：
/// - 固定长度数组，描述工具菜单组的顺序。
///
/// 错误处理：
/// - 不返回错误；顺序由代码常量固定。
fn tray_utility_menu_order() -> [TrayUtilityMenuItem; 2] {
    [
        TrayUtilityMenuItem::DisplayLocation,
        TrayUtilityMenuItem::EventMonitor,
    ]
}

/// 依据当前显示器列表把显示位置模式规范化为可用值。
///
/// 语义与边界：
/// - 若当前模式为 `Specific(selection_id)` 且该显示器已不存在，则自动回退到 `Auto`。
/// - 回退时会通过 `set_display_location_mode` 同步更新全局状态与刷新版本号。
///
/// 入参：
/// - `display_choices`：当前会话可用的显示器选项列表。
///
/// 返回值：
/// - 在当前显示器快照下实际可用的显示位置模式。
///
/// 错误处理：
/// - 不返回错误；显示器缺失时自动回退。
fn normalized_display_location_mode(display_choices: &[DisplayChoice]) -> DisplayLocationMode {
    match current_display_location_mode() {
        DisplayLocationMode::Auto => DisplayLocationMode::Auto,
        DisplayLocationMode::Specific(selection_id) => {
            if display_choices
                .iter()
                .any(|choice| choice.selection_id == selection_id)
            {
                DisplayLocationMode::Specific(selection_id)
            } else {
                set_display_location_mode(DisplayLocationMode::Auto);
                DisplayLocationMode::Auto
            }
        }
    }
}

/// 在托盘菜单弹出前刷新“显示位置”子菜单。
///
/// 语义与边界：
/// - 子菜单内容基于当前 `NSScreen` 快照实时重建。
/// - 若本次运行从未见过多显示器且当前仍为单显示器，则父菜单保持隐藏。
/// - 一旦本次运行曾经见过多显示器，父菜单就保持可见；退回单显示器时显示说明项。
///
/// 错误处理：
/// - 如果父菜单、子菜单或菜单处理对象尚未初始化，则静默跳过。
///
/// 关键副作用：
/// - 会读当前显示器列表、重建 Cocoa 子菜单，并可能把缺失的手动选择回退到 `Auto`。
fn refresh_display_location_menu() {
    let display_choices = crate::cat::current_display_choices();
    let display_count = display_choices.len();
    if display_count > 1 {
        DISPLAY_LOCATION_MENU_ACTIVATED.store(true, Ordering::Relaxed);
    }
    let should_show = should_show_display_location_menu(
        DISPLAY_LOCATION_MENU_ACTIVATED.load(Ordering::Relaxed),
        display_count,
    );
    let active_mode = normalized_display_location_mode(&display_choices);

    unsafe {
        let parent_ptr = DISPLAY_LOCATION_MENU_ITEM_PTR.load(Ordering::Acquire);
        let submenu_ptr = DISPLAY_LOCATION_SUBMENU_PTR.load(Ordering::Acquire);
        let handler_ptr = MENU_HANDLER_PTR.load(Ordering::Acquire);
        if parent_ptr.is_null() || submenu_ptr.is_null() || handler_ptr.is_null() {
            return;
        }

        let _: () = msg_send![parent_ptr, setHidden: !should_show];
        if !should_show {
            return;
        }

        let _: () = msg_send![submenu_ptr, removeAllItems];
        let mtm = MainThreadMarker::new_unchecked();

        let auto_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(localized_menu_title(TranslationKey::Automatic)),
            Some(sel!(selectDisplayLocationAuto:)),
            &NSString::from_str(""),
        );
        let _: () = msg_send![&*auto_item, setTarget: handler_ptr];
        let auto_state: isize = if active_mode == DisplayLocationMode::Auto {
            1
        } else {
            0
        };
        let _: () = msg_send![&*auto_item, setState: auto_state];
        let _: () = msg_send![submenu_ptr, addItem: &*auto_item];
        std::mem::forget(auto_item);

        if display_count > 1 {
            let separator = NSMenuItem::separatorItem(mtm);
            let _: () = msg_send![submenu_ptr, addItem: &*separator];
            std::mem::forget(separator);

            for choice in &display_choices {
                let item = NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc(),
                    &NSString::from_str(&choice.label),
                    Some(sel!(selectDisplayLocation:)),
                    &NSString::from_str(""),
                );
                let _: () = msg_send![&*item, setTarget: handler_ptr];
                let represented = NSString::from_str(&choice.selection_id);
                let _: () = msg_send![&*item, setRepresentedObject: &*represented];
                let state: isize =
                    if active_mode == DisplayLocationMode::Specific(choice.selection_id.clone()) {
                        1
                    } else {
                        0
                    };
                let _: () = msg_send![&*item, setState: state];
                let _: () = msg_send![submenu_ptr, addItem: &*item];
                std::mem::forget(item);
            }
        } else {
            let info_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                mtm.alloc(),
                &NSString::from_str(localized_menu_title(TranslationKey::OnlyOneDisplayDetected)),
                None,
                &NSString::from_str(""),
            );
            let _: () = msg_send![&*info_item, setEnabled: false];
            let _: () = msg_send![submenu_ptr, addItem: &*info_item];
            std::mem::forget(info_item);
        }
    }
}

// ── 帧预计算 ──

pub fn precompute_tray_frames(sprite: &DynamicImage) -> Vec<Vec<Vec<u8>>> {
    let mut all = Vec::new();
    for anim in TRAY_ANIMS {
        let mut frames = Vec::new();
        for f in 0..anim.frames {
            frames.push(extract_tray_frame_as_png(sprite, anim, f));
        }
        all.push(frames);
    }
    all
}

fn extract_tray_frame_as_png(sprite: &DynamicImage, anim: &TrayAnimDef, frame_idx: u32) -> Vec<u8> {
    let src_x = frame_idx * FRAME_SIZE + anim.x;
    let src_y = anim.row * FRAME_SIZE + anim.y;
    let sub = sprite.crop_imm(src_x, src_y, anim.w, anim.h);

    let scaled_w = anim.w * PIXEL_SCALE;
    let scaled_h = anim.h * PIXEL_SCALE;
    let canvas_w = CANVAS_W * PIXEL_SCALE;
    let canvas_h = CANVAS_H * PIXEL_SCALE;

    let offset_x = canvas_w.saturating_sub(scaled_w) / 2;
    let offset_y = if anim.h >= CANVAS_H {
        0
    } else {
        (CANVAS_H - anim.h) * PIXEL_SCALE
    };

    let mut canvas = RgbaImage::new(canvas_w, canvas_h);

    for dy in 0..scaled_h {
        for dx in 0..scaled_w {
            let sx = dx / PIXEL_SCALE;
            let sy = dy / PIXEL_SCALE;
            if sx < anim.w && sy < anim.h {
                let pixel = sub.get_pixel(sx, sy);
                let cx = offset_x + dx;
                let cy = offset_y + dy;
                if cx < canvas_w && cy < canvas_h {
                    canvas.put_pixel(cx, cy, pixel);
                }
            }
        }
    }

    let mut png_buf = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_buf);
    encoder
        .write_image(
            canvas.as_raw(),
            canvas_w,
            canvas_h,
            image::ExtendedColorType::Rgba8,
        )
        .expect("PNG encode failed");
    png_buf
}

// ── NSImage 创建 ──

unsafe fn png_to_nsimage(png: &[u8]) -> Retained<NSImage> {
    let canvas_w = CANVAS_W * PIXEL_SCALE;
    let canvas_h = CANVAS_H * PIXEL_SCALE;
    let logical_w = canvas_w as f64 / 2.0;
    let logical_h = canvas_h as f64 / 2.0;

    let data = NSData::with_bytes(png);
    let image =
        NSImage::initWithData(NSImage::alloc(), &data).expect("Failed to create NSImage from PNG");
    image.setSize(NSSize::new(logical_w, logical_h));
    image.setTemplate(false);
    image
}

pub fn create_tray_nsimages(png_frames: &[Vec<Vec<u8>>]) -> Vec<Vec<Retained<NSImage>>> {
    let mut all = Vec::new();
    for anim_frames in png_frames {
        let mut images = Vec::new();
        for png in anim_frames {
            images.push(unsafe { png_to_nsimage(png) });
        }
        all.push(images);
    }
    all
}

// ── NSStatusBar 创建 ──

/// 创建并初始化托盘 `NSStatusItem`。
///
/// 入参：
/// - `nsimages`: 预先解码好的托盘动画帧，按动画索引和帧索引组织；必须至少包含睡眠动画首帧。
///
/// 返回值：
/// - 已完成按钮、菜单与点击处理绑定的 `NSStatusItem`。
///
/// 错误处理：
/// - 当前实现不返回 `Result`；若关键 Cocoa 调用失败，将遵循底层绑定行为并可能直接 panic。
///
/// 关键副作用：
/// - 在主线程创建系统托盘图标与右键菜单。
/// - 会缓存若干菜单项指针，用于后续刷新勾选状态和国际化标题。
/// - 左键点击会切换猫窗口可见性，右键点击会弹出托盘菜单。
pub fn create_status_item(nsimages: &[Vec<Retained<NSImage>>]) -> Retained<NSStatusItem> {
    unsafe {
        let mtm = MainThreadMarker::new_unchecked();
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(-1.0);

        // 设置初始图标
        if let Some(button) = status_item.button(mtm) {
            let initial = &nsimages[ANIM_SLEEP][0];
            button.setImage(Some(initial));

            // tooltip
            button.setToolTip(Some(&NSString::from_str("Claude Cat")));

            // 设置 sendActionOn: leftMouseUp | rightMouseUp (4 | 16 = 20)
            let mask: usize = (1 << 2) | (1 << 4);
            let _: isize = msg_send![&*button, sendActionOn: mask];

            // 创建点击处理对象（retain 返回 id，非 void）
            let handler_class = register_tray_handler_class();
            let handler: *mut AnyObject = msg_send![handler_class, new];
            // retain 使其在应用生命周期内有效（返回 id）
            let _: *mut AnyObject = msg_send![handler, retain];

            // 设置 target + action
            let _: () = msg_send![&*button, setTarget: handler];
            let _: () = msg_send![&*button, setAction: sel!(trayClicked:)];
        }

        // 创建右键菜单（不设置到 status_item 上，手动弹出）
        let menu = NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(""));

        // 获取 handler 实例用于 cc/cx 菜单项的 target
        let handler_class = register_tray_handler_class();
        let menu_handler: *mut AnyObject = msg_send![handler_class, new];
        let _: *mut AnyObject = msg_send![menu_handler, retain];
        MENU_HANDLER_PTR.store(menu_handler, Ordering::Release);

        // cc 开关
        let cc_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Claude Code"),
            Some(sel!(toggleCC:)),
            &NSString::from_str(""),
        );
        let _: () = msg_send![&*cc_item, setTarget: menu_handler];
        let _: () = msg_send![&*cc_item, setState: 1_isize]; // 初始勾选
        menu.addItem(&cc_item);
        CC_MENU_ITEM_PTR.store(&*cc_item as *const _ as *mut AnyObject, Ordering::Release);
        std::mem::forget(cc_item);

        // cx 开关
        let cx_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("Codex"),
            Some(sel!(toggleCX:)),
            &NSString::from_str(""),
        );
        let _: () = msg_send![&*cx_item, setTarget: menu_handler];
        let _: () = msg_send![&*cx_item, setState: 1_isize]; // 初始勾选
        menu.addItem(&cx_item);
        CX_MENU_ITEM_PTR.store(&*cx_item as *const _ as *mut AnyObject, Ordering::Release);
        std::mem::forget(cx_item);

        // 分隔线
        let sep1 = NSMenuItem::separatorItem(mtm);
        menu.addItem(&sep1);

        for utility_item in tray_utility_menu_order() {
            match utility_item {
                TrayUtilityMenuItem::DisplayLocation => {
                    let display_location_submenu =
                        NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(""));
                    let display_location_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                        mtm.alloc(),
                        &NSString::from_str(localized_menu_title(TranslationKey::DisplayLocation)),
                        None,
                        &NSString::from_str(""),
                    );
                    let _: () =
                        msg_send![&*display_location_item, setSubmenu: &*display_location_submenu];
                    let _: () = msg_send![&*display_location_item, setHidden: true];
                    menu.addItem(&display_location_item);
                    DISPLAY_LOCATION_MENU_ITEM_PTR.store(
                        &*display_location_item as *const _ as *mut AnyObject,
                        Ordering::Release,
                    );
                    DISPLAY_LOCATION_SUBMENU_PTR.store(
                        &*display_location_submenu as *const _ as *mut AnyObject,
                        Ordering::Release,
                    );
                    std::mem::forget(display_location_submenu);
                    std::mem::forget(display_location_item);
                }
                TrayUtilityMenuItem::EventMonitor => {
                    let gui_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                        mtm.alloc(),
                        &NSString::from_str(localized_menu_title(TranslationKey::EventMonitor)),
                        Some(sel!(openGui:)),
                        &NSString::from_str("e"),
                    );
                    let _: () = msg_send![&*gui_item, setTarget: menu_handler];
                    menu.addItem(&gui_item);
                    GUI_MENU_ITEM_PTR
                        .store(&*gui_item as *const _ as *mut AnyObject, Ordering::Release);
                    std::mem::forget(gui_item);
                }
            }
        }

        // 分隔线
        let sep2 = NSMenuItem::separatorItem(mtm);
        menu.addItem(&sep2);

        // 退出
        let quit_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(localized_menu_title(TranslationKey::Quit)),
            Some(sel!(quitApp:)),
            &NSString::from_str(""),
        );
        let _: () = msg_send![&*quit_item, setTarget: menu_handler];
        menu.addItem(&quit_item);
        QUIT_MENU_ITEM_PTR.store(&*quit_item as *const _ as *mut AnyObject, Ordering::Release);
        std::mem::forget(quit_item);

        // 保存 menu 指针（泄漏 Retained 防止释放）
        let menu_raw = &*menu as *const _ as *mut AnyObject;
        MENU_PTR.store(menu_raw, Ordering::Release);
        std::mem::forget(menu);

        // 保存 status_item 指针（用于 popUpStatusItemMenu 定位）
        let item_raw = &*status_item as *const _ as *mut AnyObject;
        STATUS_ITEM_PTR.store(item_raw, Ordering::Release);

        status_item
    }
}

// ── 动画推进 ──

pub fn advance_tray_animation(state: &mut TrayAnimState) {
    let anim = &TRAY_ANIMS[state.current_anim];
    state.frame_index = (state.frame_index + 1) % anim.frames;

    if state.state == TrayState::Waking && state.frame_index == 0 {
        state.stretch_count += 1;
        if state.stretch_count >= 2 {
            state.state = TrayState::Active;
            let (anim_idx, dur) = weighted_random_anim(state.current_anim);
            state.current_anim = anim_idx;
            state.frame_index = 0;
            state.anim_start_ms = now_ms();
            state.current_anim_duration = dur;
        }
    }
}

pub fn sync_tray_state(state: &mut TrayAnimState, claude_state: ClaudeState) {
    let has_active = claude_state == ClaudeState::Active || claude_state == ClaudeState::Idle;
    let now = now_ms();

    match state.state {
        TrayState::Sleeping => {
            if has_active {
                state.state = TrayState::Waking;
                state.current_anim = ANIM_STRETCH;
                state.frame_index = 0;
                state.stretch_count = 0;
                state.anim_start_ms = now;
            }
        }
        TrayState::Waking => {
            if !has_active {
                state.state = TrayState::Sleeping;
                state.current_anim = ANIM_SLEEP;
                state.frame_index = 0;
            }
        }
        TrayState::Active => {
            if !has_active {
                state.state = TrayState::Sleeping;
                state.current_anim = ANIM_SLEEP;
                state.frame_index = 0;
            } else if now - state.anim_start_ms >= state.current_anim_duration {
                let (anim_idx, dur) = weighted_random_anim(state.current_anim);
                state.current_anim = anim_idx;
                state.frame_index = 0;
                state.anim_start_ms = now;
                state.current_anim_duration = dur;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn display_location_menu_visibility_should_stay_enabled_after_multidisplay_seen() {
        assert!(!super::should_show_display_location_menu(false, 1));
        assert!(super::should_show_display_location_menu(false, 2));
        assert!(super::should_show_display_location_menu(true, 1));
    }

    #[test]
    fn tray_utility_menu_order_should_place_display_location_next_to_event_monitor() {
        assert_eq!(
            super::tray_utility_menu_order(),
            [
                super::TrayUtilityMenuItem::DisplayLocation,
                super::TrayUtilityMenuItem::EventMonitor,
            ]
        );
    }
}

pub fn update_tray_icon(
    status_item: &NSStatusItem,
    state: &TrayAnimState,
    nsimages: &[Vec<Retained<NSImage>>],
) {
    let anim_idx = state.current_anim;
    let frame_idx = state.frame_index as usize;

    if anim_idx < nsimages.len() && frame_idx < nsimages[anim_idx].len() {
        let image = &nsimages[anim_idx][frame_idx];
        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            if let Some(button) = status_item.button(mtm) {
                button.setImage(Some(image));
            }
        }
    }
}

// ── 工具函数 ──

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn weighted_random_anim(current: usize) -> (usize, u64) {
    let pool: Vec<&ActiveAnimEntry> = ACTIVE_ANIMS.iter().filter(|a| a.index != current).collect();

    let total_weight: u32 = pool.iter().map(|a| a.weight).sum();
    if total_weight == 0 {
        return (0, 15000);
    }

    let mut r = (now_ms() % total_weight as u64) as u32;
    for aa in &pool {
        if r < aa.weight {
            return (aa.index, aa.duration_ms);
        }
        r -= aa.weight;
    }
    (pool[0].index, pool[0].duration_ms)
}
