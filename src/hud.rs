//! The A.R.I.A HUD: a borderless window with a transparent background.
//!
//! It is an ordinary window, not one pinned on top, so it can sit over other
//! apps and other apps can sit over it. Drag it with the mouse to move it and
//! press Esc to close it. Its app id is [`APP_ID`], so a window manager rule
//! can match it (on Hyprland: `windowrule = float, class:^(aria-hud)$`).

use eframe::egui;

use crate::error::{AriaError, Result};

/// The Wayland app id / X11 class of the HUD window.
pub const APP_ID: &str = "aria-hud";

#[derive(Default)]
struct Hud;

impl eframe::App for Hud {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully transparent: only what is drawn below is visible.
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            // The whole window is a drag handle, since it has no title bar.
            let drag = ui.interact(ui.max_rect(), ui.scope_id().with("drag"), egui::Sense::drag());
            if drag.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Hello, world")
                        .size(48.0)
                        .strong()
                        .color(egui::Color32::from_rgb(0x7f, 0xdb, 0xff)),
                );
            });
        });
    }
}

/// Opens the HUD and blocks until it is closed.
pub fn run() -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id(APP_ID)
            .with_title("A.R.I.A HUD")
            .with_inner_size([480.0, 160.0])
            .with_decorations(false)
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native("A.R.I.A HUD", options, Box::new(|_cc| Ok(Box::new(Hud))))
        .map_err(|e| AriaError::Other(format!("could not open the HUD: {e}")))
}
