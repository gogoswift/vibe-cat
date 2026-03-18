//! Windows 系统托盘图标 -- 使用 tray-icon crate
#![cfg(target_os = "windows")]

use std::sync::atomic::Ordering;

use image::GenericImageView;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::cat::{CAT_VISIBLE, CC_ENABLED, CX_ENABLED};

const FRAME_SIZE: u32 = 32;
const ICON_SIZE: u32 = 32;

/// 从精灵图提取一帧并转为 tray-icon 的 Icon
fn extract_tray_icon(sprite: &image::DynamicImage, row: u32, col: u32) -> Icon {
    let src_x = col * FRAME_SIZE + 9;
    let src_y = row * FRAME_SIZE + 20;
    let w = 12;
    let h = 12;
    let sub = sprite.crop_imm(src_x, src_y, w, h);

    let resized = image::imageops::resize(
        &sub.to_rgba8(),
        ICON_SIZE,
        ICON_SIZE,
        image::imageops::FilterType::Nearest,
    );

    let rgba = resized.into_raw();
    Icon::from_rgba(rgba, ICON_SIZE, ICON_SIZE).expect("Failed to create tray icon")
}

/// 启动 Windows 托盘图标（在后台线程中运行）
pub fn setup_tray() {
    std::thread::spawn(|| {
        let sprite = image::load_from_memory(crate::cat::CAT_SPRITE_BYTES)
            .expect("Failed to decode sprite for tray");

        // sleep 动画第一帧 (row=6)
        let icon = extract_tray_icon(&sprite, 6, 0);

        let menu = Menu::new();
        let cc_item = MenuItem::new("Claude Code", true, None);
        let cx_item = MenuItem::new("Codex", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        let _ = menu.append(&cc_item);
        let _ = menu.append(&cx_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&quit_item);

        let _tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("VibeCat")
            .with_icon(icon)
            .build()
            .expect("Failed to create tray icon");

        let cc_id = cc_item.id().clone();
        let cx_id = cx_item.id().clone();
        let quit_id = quit_item.id().clone();

        loop {
            if let Ok(event) = MenuEvent::receiver().recv() {
                if event.id == cc_id {
                    CC_ENABLED.fetch_xor(true, Ordering::Relaxed);
                } else if event.id == cx_id {
                    CX_ENABLED.fetch_xor(true, Ordering::Relaxed);
                } else if event.id == quit_id {
                    std::process::exit(0);
                }
            }
        }
    });
}
