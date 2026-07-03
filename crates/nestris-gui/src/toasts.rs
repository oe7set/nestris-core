//! Small top-right toast notifications with auto-expiry.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

struct Toast {
    text: String,
    kind: ToastKind,
    expires: Instant,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>) {
        let ttl = match kind {
            ToastKind::Error => Duration::from_secs(8),
            _ => Duration::from_secs(4),
        };
        self.items.push(Toast {
            text: text.into(),
            kind,
            expires: Instant::now() + ttl,
        });
        if self.items.len() > 6 {
            self.items.remove(0);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.items.retain(|t| t.expires > now);
        if self.items.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 40.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                for toast in &self.items {
                    let (bg, fg) = match toast.kind {
                        ToastKind::Info => (
                            egui::Color32::from_rgb(0x20, 0x30, 0x40),
                            egui::Color32::from_rgb(0xa8, 0xd8, 0xff),
                        ),
                        ToastKind::Success => (
                            egui::Color32::from_rgb(0x14, 0x32, 0x1c),
                            egui::Color32::from_rgb(0x86, 0xef, 0xac),
                        ),
                        ToastKind::Error => (
                            egui::Color32::from_rgb(0x40, 0x16, 0x16),
                            egui::Color32::from_rgb(0xfc, 0xa5, 0xa5),
                        ),
                    };
                    egui::Frame::window(ui.style())
                        .fill(bg)
                        .corner_radius(6.0)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.colored_label(fg, &toast.text);
                        });
                    ui.add_space(4.0);
                }
            });
        // Keep repainting while toasts are visible so they expire on time.
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}
