//! Canonical 256×240 preview, nearest-neighbor scaled (authentically
//! pixelated, like the web GUI's `image-rendering: pixelated` canvas).

use core::pin::Pin;

use cxx_qt_lib::{QImage, QImageFormat, QPainterRenderHint, QRect};

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
        type CanonFrameView = super::CanonFrameViewRust;

        /// Paint the canonical frame (called by the scene graph).
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
    impl cxx_qt::Initialize for CanonFrameView {}
}

#[derive(Default)]
pub struct CanonFrameViewRust;

impl qobject::CanonFrameView {
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

        let store = frames::FRAMES.lock().unwrap();
        let Some(canon) = &store.canon else {
            return;
        };
        let (fw, fh) = (256.0f64, 240.0f64);
        if vw <= 0.0 || vh <= 0.0 {
            return;
        }

        // SAFETY: canonical frames are exactly 256*240*4 RGBA bytes.
        let image = unsafe {
            QImage::from_raw_bytes(canon.clone(), 256, 240, QImageFormat::Format_RGBA8888)
        };

        // Nearest-neighbor: leave SmoothPixmapTransform off.
        painter
            .as_mut()
            .set_render_hint(QPainterRenderHint::SmoothPixmapTransform, false);

        let scale = (vw / fw).min(vh / fh);
        let (dw, dh) = (fw * scale, fh * scale);
        let (dx, dy) = ((vw - dw) / 2.0, (vh - dh) / 2.0);
        painter.as_mut().draw_image(
            &QRect::new(dx as i32, dy as i32, dw as i32, dh as i32),
            &image,
        );
    }

    pub fn refresh(self: Pin<&mut Self>) {
        self.update();
    }
}

impl cxx_qt::Initialize for qobject::CanonFrameView {
    fn initialize(self: Pin<&mut Self>) {}
}
