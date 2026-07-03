//! The NestrisChamps-style statistics window: score/pace tiles, LINES and
//! POINTS breakdowns, TRT trend, piece distribution with droughts, the
//! HEIGHT & STATE timeline, and the persistent PB tables.
//!
//! All charts are hand-painted with `egui::Painter` — the marks are simple
//! enough that a plotting dependency would cost more than it saves.

use egui::{Align2, Color32, FontId, Rect, Stroke, pos2, vec2};
use nestris_engine::output::OutputFrame;
use nestris_engine::stats_ext::{
    ExtendedStats, FLAG_CLEAN_SLOPE, FLAG_DOUBLE_WELL, FLAG_IN_DROUGHT, FLAG_TETRIS_READY,
    PIECE_ORDER,
};

use crate::session::SessionStore;

const ACCENT: Color32 = Color32::from_rgb(0x3c, 0xbc, 0xfc);
const GOLD: Color32 = Color32::from_rgb(0xff, 0xd7, 0x00);
const DIM: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xad);
const GRID: Color32 = Color32::from_rgb(0x2a, 0x33, 0x3d);

/// Render the statistics window content.
pub fn stats_window(
    ctx: &egui::Context,
    open: &mut bool,
    output: Option<&OutputFrame>,
    ext: Option<&ExtendedStats>,
    session: &SessionStore,
) {
    egui::Window::new("Statistics")
        .open(open)
        .default_width(560.0)
        .default_height(640.0)
        .vscroll(true)
        .show(ctx, |ui| {
            let Some(output) = output else {
                ui.label("Open a source or replay to see statistics.");
                return;
            };
            let Some(ext) = ext else {
                ui.label("No extended statistics yet.");
                return;
            };
            tile_row(ui, output, ext);
            ui.add_space(8.0);
            ui.columns(2, |cols| {
                lines_panel(&mut cols[0], output);
                points_panel(&mut cols[1], output, ext);
            });
            ui.add_space(8.0);
            trt_chart(ui, ext);
            ui.add_space(8.0);
            pieces_panel(ui, ext);
            ui.add_space(8.0);
            height_chart(ui, ext);
            ui.add_space(8.0);
            pb_tables(ui, session);
        });
}

fn fmt_opt(v: Option<i64>) -> String {
    v.map_or("—".into(), |v| v.to_string())
}

fn tile(ui: &mut egui::Ui, label: &str, value: String, color: Color32) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(label).small().color(DIM));
                ui.label(
                    egui::RichText::new(value)
                        .font(FontId::monospace(20.0))
                        .color(color),
                );
            });
        });
}

fn tile_row(ui: &mut egui::Ui, output: &OutputFrame, ext: &ExtendedStats) {
    let s = &output.stats;
    ui.horizontal_wrapped(|ui| {
        tile(
            ui,
            "SCORE",
            fmt_opt(output.fields.score),
            Color32::WHITE,
        );
        tile(
            ui,
            "PACE",
            ext.pace_score.map_or("—".into(), |v| v.to_string()),
            ACCENT,
        );
        tile(ui, "LINES", fmt_opt(output.fields.lines), Color32::WHITE);
        tile(ui, "LEVEL", fmt_opt(output.fields.level), Color32::WHITE);
        tile(
            ui,
            "EFF",
            ext.efficiency.map_or("—".into(), |v| format!("{v:.0}")),
            ACCENT,
        );
        tile(ui, "BRN", s.burn.to_string(), Color32::WHITE);
        tile(
            ui,
            "TRT",
            s.tetris_rate
                .map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
            GOLD,
        );
        tile(
            ui,
            "I-DRT",
            format!(
                "{} / {} / {}",
                ext.i_drought.current, ext.i_drought.last, ext.i_drought.max
            ),
            if ext.i_drought.current >= 13 {
                Color32::from_rgb(0xfc, 0x74, 0x60)
            } else {
                Color32::WHITE
            },
        );
    });
}

fn lines_panel(ui: &mut egui::Ui, output: &OutputFrame) {
    let c = &output.stats.clears;
    let lines = [
        ("Singles", c.single as i64, c.single as i64),
        ("Doubles", c.double as i64, c.double as i64 * 2),
        ("Triples", c.triple as i64, c.triple as i64 * 3),
        ("Tetris", c.tetris as i64, c.tetris as i64 * 4),
    ];
    let total: i64 = lines.iter().map(|(_, _, l)| l).sum();
    ui.group(|ui| {
        ui.strong(format!("LINES {total:03}"));
        egui::Grid::new("lines-grid").num_columns(3).show(ui, |ui| {
            for (label, count, contributed) in lines {
                let pct = if total > 0 {
                    format!("{:.0}%", contributed as f64 * 100.0 / total as f64)
                } else {
                    "—".into()
                };
                ui.label(label);
                ui.monospace(format!("{count:03}"));
                ui.monospace(pct);
                ui.end_row();
            }
        });
    });
}

fn points_panel(ui: &mut egui::Ui, output: &OutputFrame, ext: &ExtendedStats) {
    let p = &ext.points;
    let total = output.fields.score.unwrap_or(0).max(1);
    let rows = [
        ("Drops", p.drops),
        ("Singles", p.singles),
        ("Doubles", p.doubles),
        ("Triples", p.triples),
        ("Tetris", p.tetrises),
    ];
    ui.group(|ui| {
        ui.strong(format!("POINTS {}", output.fields.score.unwrap_or(0)));
        egui::Grid::new("points-grid").num_columns(3).show(ui, |ui| {
            for (label, points) in rows {
                ui.label(label);
                ui.monospace(format!("{points:06}"));
                ui.monospace(format!("{:.0}%", points as f64 * 100.0 / total as f64));
                ui.end_row();
            }
        });
    });
}

fn chart_frame(ui: &mut egui::Ui, title: &str, height: f32) -> (egui::Painter, Rect) {
    ui.strong(title);
    let (response, painter) = ui.allocate_painter(
        vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let rect = response.rect.shrink(4.0);
    painter.rect_filled(response.rect, 4.0, Color32::from_rgb(0x10, 0x14, 0x18));
    (painter, rect)
}

fn trt_chart(ui: &mut egui::Ui, ext: &ExtendedStats) {
    let samples = &ext.trt_trend;
    let (painter, rect) = chart_frame(ui, "TRT TREND", 90.0);
    for frac in [0.25, 0.5, 0.75] {
        let y = rect.bottom() - rect.height() * frac;
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, GRID),
        );
    }
    if samples.len() < 2 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "waiting for clears…",
            FontId::proportional(12.0),
            DIM,
        );
        return;
    }
    let x_max = samples.last().unwrap().0.max(1) as f32;
    let points: Vec<egui::Pos2> = samples
        .iter()
        .map(|&(lines, rate)| {
            pos2(
                rect.left() + rect.width() * lines as f32 / x_max,
                rect.bottom() - rect.height() * (rate as f32).clamp(0.0, 1.0),
            )
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(1.5, GOLD)));
}

fn pieces_panel(ui: &mut egui::Ui, ext: &ExtendedStats) {
    let dist = &ext.piece_dist;
    let total: i64 = dist.counts.iter().sum();
    ui.group(|ui| {
        ui.strong(format!(
            "PIECES {total:03}  —  DEV {:.1}%",
            dist.deviation * 100.0
        ));
        let max_count = dist.counts.iter().copied().max().unwrap_or(1).max(1);
        for (i, piece) in PIECE_ORDER.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.monospace(format!("{} {:03}", piece.letter(), dist.counts[i]));
                let (response, painter) = ui.allocate_painter(
                    vec2((ui.available_width() - 70.0).max(40.0), 12.0),
                    egui::Sense::hover(),
                );
                let rect = response.rect;
                painter.rect_filled(rect, 2.0, Color32::from_rgb(0x10, 0x14, 0x18));
                let w = rect.width() * dist.counts[i] as f32 / max_count as f32;
                let color = if *piece == nestris_engine::enums::Piece::I {
                    GOLD
                } else {
                    ACCENT
                };
                painter.rect_filled(
                    Rect::from_min_size(rect.min, vec2(w, rect.height())),
                    2.0,
                    color,
                );
                ui.monospace(format!("drt {:02}", dist.drought[i]));
            });
        }
    });
}

fn height_chart(ui: &mut egui::Ui, ext: &ExtendedStats) {
    let samples = &ext.height_timeline;
    let (painter, rect) = chart_frame(ui, "HEIGHT & STATE", 110.0);
    if samples.len() < 2 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "waiting for play…",
            FontId::proportional(12.0),
            DIM,
        );
        return;
    }
    let t0 = samples.first().unwrap().0;
    let t1 = samples.last().unwrap().0.max(t0 + 1.0);
    let span = (t1 - t0) as f32;
    let x_of = |ts: f64| rect.left() + rect.width() * ((ts - t0) as f32 / span);

    // Flag strips (bottom 18px): tetris-ready gold, drought red, wells blue.
    let strip_h = 5.0;
    let strip_y = rect.bottom() - 3.0 * strip_h;
    let mut prev_x = rect.left();
    for &(ts, _, flags) in samples {
        let x = x_of(ts);
        let strip = |row: f32, on: bool, color: Color32| {
            if on {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(prev_x, strip_y + row * strip_h),
                        pos2(x, strip_y + (row + 1.0) * strip_h - 1.0),
                    ),
                    0.0,
                    color,
                );
            }
        };
        strip(0.0, flags & FLAG_TETRIS_READY != 0, GOLD);
        strip(
            1.0,
            flags & FLAG_IN_DROUGHT != 0,
            Color32::from_rgb(0xfc, 0x74, 0x60),
        );
        strip(
            2.0,
            flags & (FLAG_DOUBLE_WELL | FLAG_CLEAN_SLOPE) != 0,
            ACCENT,
        );
        prev_x = x;
    }

    // Height curve above the strips.
    let curve_bottom = strip_y - 2.0;
    let curve_h = curve_bottom - rect.top();
    let points: Vec<egui::Pos2> = samples
        .iter()
        .map(|&(ts, h, _)| {
            pos2(
                x_of(ts),
                curve_bottom - curve_h * (f32::from(h) / 20.0).clamp(0.0, 1.0),
            )
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(1.5, Color32::WHITE)));
    painter.text(
        pos2(rect.left() + 2.0, rect.top()),
        Align2::LEFT_TOP,
        "ready / drought / clean",
        FontId::proportional(9.0),
        DIM,
    );
}

fn pb_tables(ui: &mut egui::Ui, session: &SessionStore) {
    let (today, _) = crate::session::local_stamp();
    ui.columns(2, |cols| {
        pb_table(&mut cols[0], "HIGH SCORES — TODAY", session.best(Some(&today), 5));
        pb_table(&mut cols[1], "HIGH SCORES — OVERALL", session.best(None, 5));
    });
}

fn pb_table(ui: &mut egui::Ui, title: &str, rows: Vec<&crate::session::GameRecord>) {
    ui.group(|ui| {
        ui.strong(title);
        if rows.is_empty() {
            ui.label(egui::RichText::new("no finished games yet").color(DIM));
            return;
        }
        egui::Grid::new(title).num_columns(4).striped(true).show(ui, |ui| {
            ui.label(egui::RichText::new("Score").small().color(DIM));
            ui.label(egui::RichText::new("Lines").small().color(DIM));
            ui.label(egui::RichText::new("Lvl").small().color(DIM));
            ui.label(egui::RichText::new("TRT").small().color(DIM));
            ui.end_row();
            for row in rows {
                ui.monospace(row.score.to_string());
                ui.monospace(row.lines.to_string());
                ui.monospace(
                    row.end_level.map_or("—".into(), |v| v.to_string()),
                );
                ui.monospace(
                    row.tetris_rate
                        .map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
                );
                ui.end_row();
            }
        });
    });
}
