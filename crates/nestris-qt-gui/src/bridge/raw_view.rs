//! Painted-item spike: proves the pure-Rust QQuickPaintedItem path
//! (raw RGBA bytes → QImage → QPainter::draw_image) that the raw,
//! canonical, and playfield live views all build on.

use core::pin::Pin;

use cxx_qt_lib::{QColor, QImage, QImageFormat, QRect, QRectF};

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qcolor.h");
        /// QColor from cxx_qt_lib
        type QColor = cxx_qt_lib::QColor;

        include!("cxx-qt-lib/qrect.h");
        /// QRect from cxx_qt_lib
        type QRect = cxx_qt_lib::QRect;

        include!("cxx-qt-lib/qrectf.h");
        /// QRectF from cxx_qt_lib
        type QRectF = cxx_qt_lib::QRectF;

        include!("cxx-qt-lib/qsizef.h");
        /// QSizeF from cxx_qt_lib
        type QSizeF = cxx_qt_lib::QSizeF;

        include!("cxx-qt-lib/qpainter.h");
        /// QPainter from cxx_qt_lib
        type QPainter = cxx_qt_lib::QPainter;
    }

    // The QtQuick base class we inherit from.
    unsafe extern "C++" {
        include!(<QtQuick/QQuickPaintedItem>);
        /// Base type for painted QML items
        type QQuickPaintedItem;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[base = QQuickPaintedItem]
        type RawFrameView = super::RawFrameViewRust;

        /// Paint the current frame (called by the scene graph).
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
    impl cxx_qt::Initialize for RawFrameView {}
}

/// Rust state behind the QML item: for the spike, a generated RGBA test
/// card standing in for a decoded video frame.
pub struct RawFrameViewRust {
    frame: QImage,
}

impl Default for RawFrameViewRust {
    fn default() -> Self {
        // 256×240 gradient test card with an NES-cyan frame border.
        let (w, h) = (256usize, 240usize);
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                if x < 4 || x >= w - 4 || y < 4 || y >= h - 4 {
                    rgba.extend_from_slice(&[0x3c, 0xbc, 0xfc, 0xff]);
                } else {
                    rgba.extend_from_slice(&[x as u8, y as u8, 0x30, 0xff]);
                }
            }
        }
        // SAFETY: buffer length is exactly w*h*4 for Format_RGBA8888.
        let frame = unsafe {
            QImage::from_raw_bytes(rgba, w as i32, h as i32, QImageFormat::Format_RGBA8888)
        };
        Self { frame }
    }
}

impl qobject::RawFrameView {
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
        painter.as_mut().fill_rect(
            &QRectF::new(0.0, 0.0, vw, vh),
            &QColor::from_rgb(10, 12, 16),
        );

        let frame = &self.frame;
        let (fw, fh) = (f64::from(frame.width()), f64::from(frame.height()));
        if fw <= 0.0 || fh <= 0.0 || vw <= 0.0 || vh <= 0.0 {
            return;
        }
        // Aspect-fit, centered — same placement the live previews use.
        let scale = (vw / fw).min(vh / fh);
        let (dw, dh) = (fw * scale, fh * scale);
        let (dx, dy) = ((vw - dw) / 2.0, (vh - dh) / 2.0);
        painter.as_mut().draw_image(
            &QRect::new(dx as i32, dy as i32, dw as i32, dh as i32),
            frame,
        );
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.update();
    }
}

impl cxx_qt::Initialize for qobject::RawFrameView {
    fn initialize(self: Pin<&mut Self>) {}
}
