//! Optional system-tray icon.
//!
//! The icon is created on the GUI thread, which is the thread winit pumps
//! Windows messages on. Menu clicks arrive on a global channel, so a small
//! watcher thread reads them and wakes egui with `request_repaint()` - that
//! works even while the window is hidden, which is the whole point.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eframe::egui;

use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

pub struct Tray {
    /// Dropping this removes the icon from the notification area.
    _icon: TrayIcon,
    show_requested: Arc<AtomicBool>,
    quit_requested: Arc<AtomicBool>,
}

impl Tray {
    /// Creates the tray icon. Returns `None` if Windows refuses, which is not
    /// worth failing the app over.
    pub fn create(ctx: egui::Context) -> Option<Tray> {
        let menu = Menu::new();
        let show_item = MenuItem::new("Show AudioPush", true, None);
        let quit_item = MenuItem::new("Quit", true, None);
        menu.append(&show_item).ok()?;
        menu.append(&quit_item).ok()?;

        let icon = Icon::from_rgba(icon_rgba(), ICON_SIZE, ICON_SIZE).ok()?;
        let tray = TrayIconBuilder::new()
            .with_tooltip("AudioPush")
            .with_menu(Box::new(menu))
            .with_icon(icon)
            .build()
            .ok()?;

        let show_requested = Arc::new(AtomicBool::new(false));
        let quit_requested = Arc::new(AtomicBool::new(false));

        let show_id = show_item.id().clone();
        let quit_id = quit_item.id().clone();
        let show_flag = Arc::clone(&show_requested);
        let quit_flag = Arc::clone(&quit_requested);
        let menu_ctx = ctx.clone();

        // Menu clicks.
        std::thread::Builder::new()
            .name("audiopush-tray-menu".into())
            .spawn(move || {
                let receiver = MenuEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if event.id == show_id {
                        show_flag.store(true, Ordering::Relaxed);
                    } else if event.id == quit_id {
                        quit_flag.store(true, Ordering::Relaxed);
                    }
                    menu_ctx.request_repaint();
                }
            })
            .ok()?;

        // Left-clicking the icon itself also restores the window.
        let click_flag = Arc::clone(&show_requested);
        let click_ctx = ctx;
        std::thread::Builder::new()
            .name("audiopush-tray-click".into())
            .spawn(move || {
                let receiver = TrayIconEvent::receiver();
                while let Ok(event) = receiver.recv() {
                    if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                        click_flag.store(true, Ordering::Relaxed);
                        click_ctx.request_repaint();
                    }
                }
            })
            .ok()?;

        Some(Tray {
            _icon: tray,
            show_requested,
            quit_requested,
        })
    }

    /// True once (per request) when the user asked to restore the window.
    pub fn take_show_request(&self) -> bool {
        self.show_requested.swap(false, Ordering::Relaxed)
    }

    /// True once (per request) when the user asked to quit.
    pub fn take_quit_request(&self) -> bool {
        self.quit_requested.swap(false, Ordering::Relaxed)
    }
}

const ICON_SIZE: u32 = 32;

/// Draws the tray icon: a filled circle with a lighter dot, which reads
/// clearly at 16x16 on both light and dark taskbars.
fn icon_rgba() -> Vec<u8> {
    let size = ICON_SIZE as i32;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let centre = (size - 1) as f32 / 2.0;
    let outer = centre - 1.0;
    let inner = outer * 0.42;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - centre;
            let dy = y as f32 - centre;
            let distance = (dx * dx + dy * dy).sqrt();
            if distance <= inner {
                pixels.extend_from_slice(&[235, 245, 255, 255]);
            } else if distance <= outer {
                // Soften the rim so the circle does not look jagged.
                let alpha = ((outer - distance).clamp(0.0, 1.0) * 255.0) as u8;
                pixels.extend_from_slice(&[38, 128, 235, alpha.max(200)]);
            } else {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    pixels
}
