//! macOS 状态栏托盘图标 — 动画猫精灵
//!
//! 从同一精灵图提取帧，预创建 NSImage，在 eframe update() 中
//! 每 250ms 推进动画帧并更新图标。
//!
//! 左键点击：切换桌面猫显示/隐藏
//! 右键点击：弹出"退出"菜单

#![cfg(target_os = "macos")]

use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use image::{DynamicImage, GenericImageView, ImageEncoder, RgbaImage};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::sel;
use objc2::ClassType;
use objc2_app_kit::{
    NSApplication, NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{MainThreadMarker, NSData, NSSize, NSString};

use crate::cat::ClaudeState;

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
    TrayAnimDef { name: "sit1",    row: 0, frames: 4, x: 9,  y: 20, w: 12, h: 12 },
    TrayAnimDef { name: "sit2",    row: 1, frames: 4, x: 9,  y: 20, w: 12, h: 12 },
    TrayAnimDef { name: "sit3",    row: 2, frames: 4, x: 9,  y: 20, w: 13, h: 12 },
    TrayAnimDef { name: "sit4",    row: 3, frames: 4, x: 9,  y: 20, w: 13, h: 12 },
    TrayAnimDef { name: "run1",    row: 4, frames: 8, x: 7,  y: 20, w: 17, h: 12 },
    TrayAnimDef { name: "run2",    row: 5, frames: 8, x: 7,  y: 19, w: 17, h: 13 },
    TrayAnimDef { name: "sleep",   row: 6, frames: 4, x: 8,  y: 24, w: 16, h: 8  },
    TrayAnimDef { name: "play",    row: 7, frames: 6, x: 10, y: 20, w: 15, h: 12 },
    TrayAnimDef { name: "pounce",  row: 8, frames: 7, x: 9,  y: 14, w: 15, h: 18 },
    TrayAnimDef { name: "stretch", row: 9, frames: 8, x: 7,  y: 21, w: 17, h: 11 },
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
    ActiveAnimEntry { index: 0, weight: 1, duration_ms: 15000 },
    ActiveAnimEntry { index: 1, weight: 1, duration_ms: 15000 },
    ActiveAnimEntry { index: 2, weight: 1, duration_ms: 15000 },
    ActiveAnimEntry { index: 3, weight: 1, duration_ms: 15000 },
];

// ── 全局状态（点击处理用） ──

/// 桌面猫可见性标志（左键点击切换）
pub static CAT_VISIBLE: AtomicBool = AtomicBool::new(true);

/// 右键菜单指针（main thread only，应用生命周期内有效）
static MENU_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());
/// NSStatusItem 指针（用于 popUpStatusItemMenu 定位到状态栏正下方）
static STATUS_ITEM_PTR: AtomicPtr<AnyObject> = AtomicPtr::new(ptr::null_mut());

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
                // 右键：在状态栏图标正下方弹出退出菜单
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

    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        let superclass = AnyClass::get("NSObject").unwrap();
        let mut builder = ClassBuilder::new("ClaudeCatTrayHandler", superclass).unwrap();
        unsafe {
            builder.add_method(
                sel!(trayClicked:),
                tray_clicked as extern "C" fn(_, _, _),
            );
        }
        builder.register()
    })
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
        .write_image(canvas.as_raw(), canvas_w, canvas_h, image::ExtendedColorType::Rgba8)
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
    let image = NSImage::initWithData(NSImage::alloc(), &data)
        .expect("Failed to create NSImage from PNG");
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

/// 创建 NSStatusItem，设置左键/右键点击处理
/// 左键：切换猫可见性  右键：弹出"退出"菜单
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

        // 只添加"退出"
        let quit_item = NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str("退出"),
            Some(sel!(terminate:)),
            &NSString::from_str(""),
        );
        menu.addItem(&quit_item);

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
    let pool: Vec<&ActiveAnimEntry> = ACTIVE_ANIMS
        .iter()
        .filter(|a| a.index != current)
        .collect();

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
