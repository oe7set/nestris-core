//! Tracked 10×20 playfield in the authentic NES level palette, with the
//! falling piece drawn on top in its guideline color (a direct port of
//! the egui GUI's playfield renderer).

use core::pin::Pin;
use std::collections::HashSet;

use cxx_qt_lib::{QColor, QRectF};
use nestris_engine::enums::Piece;
use nestris_engine::nes_palette;

use crate::frames;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qsizef.h");
        /// QSizeF from cxx_qt_lib
        type QSizeF = cxx_qt_lib::QSizeF;

        include!("cxx-qt-lib/qpainter.h");
        /// QPainter from cxx_qt_lib
        type QPainter = cxx_qt_lib::QPainter;
    }

    unsafe extern "C++" {
        include!(<QtQuick/QQuickPaintedItem>);
        /// Base type for painted QML items
        type QQuickPaintedItem;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QQuickPaintedItem]
        type PlayfieldView = super::PlayfieldViewRust;

        /// Paint the tracked field (called by the scene graph).
        #[qinvokable]
        #[cxx_override]
        unsafe fn paint(self: Pin<&mut Self>, painter: *mut QPainter);

        #[inherit]
        fn size(self: &Self) -> QSizeF;

        #[inherit]
        fn update(self: Pin<&mut Self>);

        /// Schedule a repaint (a new frame arrived).
        #[qinvokable]
        fn refresh(self: Pin<&mut Self>);
    }

    // Constructor without a QObject* parent (the base takes QQuickItem*).
    impl cxx_qt::Initialize for PlayfieldView {}
}

#[derive(Default)]
pub struct PlayfieldViewRust;

impl qobject::PlayfieldView {
    /// # Safety
    ///
    /// `painter` is valid for the duration of the paint call.
    pub unsafe fn paint(self: Pin<&mut Self>, painter: *mut qobject::QPainter) {
        let Some(painter) = (unsafe { painter.as_mut() }) else {
            return;
        };
        // SAFETY: QPainter is never moved out of the pin.
        let mut painter = unsafe { Pin::new_unchecked(painter) };

        let size = self.as_ref().size();
        let (vw, vh) = (size.width(), size.height());
        painter
            .as_mut()
            .fill_rect(&QRectF::new(0.0, 0.0, vw, vh), &QColor::from_rgb(0, 0, 0));

        let store = frames::FRAMES.lock().unwrap();
        let Some(grid) = &store.grid else {
            return;
        };
        let cw = vw / 10.0;
        let ch = vh / 20.0;
        let piece_cells: HashSet<(u32, u32)> = store.piece_cells.iter().copied().collect();
        let cell_rect = |r: usize, c: usize| {
            QRectF::new(
                c as f64 * cw + 1.0,
                r as f64 * ch + 1.0,
                (cw - 2.0).max(1.0),
                (ch - 2.0).max(1.0),
            )
        };

        // Settled cells in the level's palette pair.
        for (r, row) in grid.iter().enumerate() {
            for (c, &id) in row.iter().enumerate() {
                if id == 0 || piece_cells.contains(&(r as u32, c as u32)) {
                    continue;
                }
                let (cr, cg, cb) = nes_palette::cell_color(store.level, id);
                painter.as_mut().fill_rect(
                    &cell_rect(r, c),
                    &QColor::from_rgb(cr.into(), cg.into(), cb.into()),
                );
            }
        }

        // Falling piece in its guideline color, on top.
        if let Some(piece) = store.piece
            && piece != Piece::None
        {
            let (cr, cg, cb) = nes_palette::piece_color(Some(piece));
            let color = QColor::from_rgb(cr.into(), cg.into(), cb.into());
            for &(r, c) in &store.piece_cells {
                painter
                    .as_mut()
                    .fill_rect(&cell_rect(r as usize, c as usize), &color);
            }
        }
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.update();
    }
}

impl cxx_qt::Initialize for qobject::PlayfieldView {
    fn initialize(self: Pin<&mut Self>) {}
}
