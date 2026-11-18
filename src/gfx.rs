//! Drawing toolkit for the native UI: DirectWrite
//! text, WIC image decoding (favicons) and the line icons.

use windows::{
    Win32::{
        Graphics::{
            Direct2D::{Common::*, *},
            DirectWrite::*,
            Imaging::*,
        },
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    },
    core::*,
};

pub fn rgba(hex: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: alpha,
    }
}

pub fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

pub fn contains(r: &D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    pub wic: IWICImagingFactory,
    pub round: ID2D1StrokeStyle,
}

impl Gfx {
    /// `d2d` must be the factory of the device the UI draws with: geometry
    /// and stroke styles from another factory cannot be drawn.
    pub fn new(d2d: ID2D1Factory) -> Result<Self> {
        unsafe {
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let round = d2d.CreateStrokeStyle(
                &D2D1_STROKE_STYLE_PROPERTIES {
                    startCap: D2D1_CAP_STYLE_ROUND,
                    endCap: D2D1_CAP_STYLE_ROUND,
                    dashCap: D2D1_CAP_STYLE_ROUND,
                    lineJoin: D2D1_LINE_JOIN_ROUND,
                    ..Default::default()
                },
                None,
            )?;
            Ok(Gfx { d2d, dwrite, wic, round })
        }
    }

    /// Single-line text style that ends in "…" when it does not fit.
    pub fn text_format(&self, size: f32, weight: DWRITE_FONT_WEIGHT) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dwrite.CreateTextFormat(
                w!("Segoe UI Variable Text"),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let sign = self.dwrite.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(
                &DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 },
                &sign,
            )?;
            Ok(f)
        }
    }

    /// Decodes PNG/ICO bytes into (color, grayscale) bitmaps ready to draw.
    pub fn decode_icon(&self, bytes: &[u8]) -> Option<(IWICBitmap, IWICBitmap)> {
        unsafe {
            let stream = self.wic.CreateStream().ok()?;
            stream.InitializeFromMemory(bytes).ok()?;
            let decoder = self.wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
            let frame = decoder.GetFrame(0).ok()?;
            let convert = |src: &IWICBitmapSource, fmt: &GUID| -> Option<IWICBitmapSource> {
                let c = self.wic.CreateFormatConverter().ok()?;
                c.Initialize(src, fmt, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
                c.cast().ok()
            };
            let frame: IWICBitmapSource = frame.cast().ok()?;
            let color = convert(&frame, &GUID_WICPixelFormat32bppPBGRA)?;
            let gray = convert(&convert(&frame, &GUID_WICPixelFormat8bppGray)?, &GUID_WICPixelFormat32bppPBGRA)?;
            // Copy into memory so the source bytes can be dropped.
            let color = self.wic.CreateBitmapFromSource(&color, WICBitmapCacheOnLoad).ok()?;
            let gray = self.wic.CreateBitmapFromSource(&gray, WICBitmapCacheOnLoad).ok()?;
            Some((color, gray))
        }
    }
}

/// Line icons, drawn in a 16×16 box at (x, y) with round caps.
#[derive(Clone, Copy)]
pub enum Icon {
    Back,
    Forward,
    Reload,
    Close,
    Plus,
    Globe,
    Search,
    Gear,
    Minimize,
    Maximize,
    Restore,
    Audio,
    Star,
    StarFilled,
    ChevronDown,
    Puzzle,
    Download,
}

pub fn draw_icon(g: &Gfx, rt: &ID2D1RenderTarget, icon: Icon, x: f32, y: f32, brush: &ID2D1Brush) {
    let p = |px: f32, py: f32| windows_numerics::Vector2 { X: x + px, Y: y + py };
    let w = 1.6;
    let line = |a: (f32, f32), b: (f32, f32)| unsafe {
        rt.DrawLine(p(a.0, a.1), p(b.0, b.1), brush, w, &g.round);
    };
    match icon {
        Icon::Back => {
            line((10.0, 3.5), (5.5, 8.0));
            line((5.5, 8.0), (10.0, 12.5));
        }
        Icon::Forward => {
            line((6.0, 3.5), (10.5, 8.0));
            line((10.5, 8.0), (6.0, 12.5));
        }
        Icon::Close => {
            line((4.5, 4.5), (11.5, 11.5));
            line((11.5, 4.5), (4.5, 11.5));
        }
        Icon::Plus => {
            line((8.0, 3.5), (8.0, 12.5));
            line((3.5, 8.0), (12.5, 8.0));
        }
        Icon::Reload => unsafe {
            // 300° arc with an arrow head at its start.
            if let Ok(path) = g.d2d.CreatePathGeometry()
                && let Ok(sink) = path.Open()
            {
                let (cx, cy, r) = (8.0, 8.0, 5.0);
                let start = p(cx + r, cy);
                let a = 300f32.to_radians();
                let end = p(cx + r * a.cos(), cy - r * a.sin());
                sink.BeginFigure(start, D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: end,
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_LARGE,
                });
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                let _ = sink.Close();
                rt.DrawGeometry(&path, brush, w, &g.round);
            }
            line((13.0, 4.5), (13.0, 8.0));
            line((13.0, 8.0), (9.5, 8.0));
        },
        Icon::Minimize => line((4.0, 8.5), (12.0, 8.5)),
        Icon::ChevronDown => {
            line((4.0, 6.5), (8.0, 10.5));
            line((8.0, 10.5), (12.0, 6.5));
        }
        Icon::Maximize => unsafe {
            let r = D2D1_ROUNDED_RECT { rect: rect(x + 4.0, y + 4.0, x + 12.0, y + 12.0), radiusX: 2.0, radiusY: 2.0 };
            rt.DrawRoundedRectangle(&r, brush, 1.4, &g.round);
        },
        Icon::Restore => unsafe {
            let r = D2D1_ROUNDED_RECT { rect: rect(x + 4.0, y + 6.0, x + 10.0, y + 12.0), radiusX: 1.5, radiusY: 1.5 };
            rt.DrawRoundedRectangle(&r, brush, 1.4, &g.round);
            line((6.5, 4.0), (12.0, 4.0));
            line((12.0, 4.0), (12.0, 9.5));
        },
        Icon::Star | Icon::StarFilled => unsafe {
            // Five-point star, rounded joins.
            if let Ok(path) = g.d2d.CreatePathGeometry()
                && let Ok(sink) = path.Open()
            {
                let (cx, cy, ro, ri) = (8.0, 8.4, 6.6, 2.9);
                for k in 0..10 {
                    let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
                    let r = if k % 2 == 0 { ro } else { ri };
                    let pt = p(cx + r * a.cos(), cy + r * a.sin());
                    if k == 0 {
                        sink.BeginFigure(pt, D2D1_FIGURE_BEGIN_FILLED);
                    } else {
                        sink.AddLine(pt);
                    }
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                let _ = sink.Close();
                if matches!(icon, Icon::StarFilled) {
                    rt.FillGeometry(&path, brush, None);
                }
                rt.DrawGeometry(&path, brush, 1.3, &g.round);
            }
        },
        Icon::Audio => unsafe {
            // Speaker with one wave.
            if let Ok(path) = g.d2d.CreatePathGeometry()
                && let Ok(sink) = path.Open()
            {
                sink.BeginFigure(p(3.0, 6.5), D2D1_FIGURE_BEGIN_FILLED);
                for (px, py) in [(5.5, 6.5), (8.5, 4.0), (8.5, 12.0), (5.5, 9.5), (3.0, 9.5)] {
                    sink.AddLine(p(px, py));
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                let _ = sink.Close();
                rt.FillGeometry(&path, brush, None);
            }
            line((11.0, 6.0), (11.8, 8.0));
            line((11.8, 8.0), (11.0, 10.0));
        },
        Icon::Gear => unsafe {
            // Outlined cog (8 rounded teeth) around a hollow hub, in the
            // same line weight as the other icons.
            if let Ok(path) = g.d2d.CreatePathGeometry()
                && let Ok(sink) = path.Open()
            {
                let (ro, ri) = (6.6f32, 4.9f32);
                let teeth = 8;
                let step = std::f32::consts::TAU / teeth as f32;
                let pt = |r: f32, a: f32| p(8.0 + r * a.cos(), 8.0 + r * a.sin());
                for k in 0..teeth {
                    let a = k as f32 * step;
                    // valley -> tooth flank -> tooth top -> flank -> next valley
                    let (v0, t0, t1, v1) = (a - step * 0.5, a - step * 0.2, a + step * 0.2, a + step * 0.5);
                    if k == 0 {
                        sink.BeginFigure(pt(ri, v0), D2D1_FIGURE_BEGIN_HOLLOW);
                    }
                    sink.AddLine(pt(ri, a - step * 0.3));
                    sink.AddLine(pt(ro, t0));
                    sink.AddLine(pt(ro, t1));
                    sink.AddLine(pt(ri, a + step * 0.3));
                    sink.AddLine(pt(ri, v1));
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                let _ = sink.Close();
                rt.DrawGeometry(&path, brush, 1.4, &g.round);
            }
            let e = D2D1_ELLIPSE { point: p(8.0, 8.0), radiusX: 2.1, radiusY: 2.1 };
            rt.DrawEllipse(&e, brush, 1.4, &g.round);
        },
        Icon::Download => {
            line((8.0, 3.0), (8.0, 10.0));
            line((5.0, 7.0), (8.0, 10.0));
            line((8.0, 10.0), (11.0, 7.0));
            line((3.5, 13.0), (12.5, 13.0));
        }
        Icon::Puzzle => unsafe {
            // Puzzle piece: a square with a knob on top and on the right.
            if let Ok(path) = g.d2d.CreatePathGeometry()
                && let Ok(sink) = path.Open()
            {
                let knob = |sink: &ID2D1GeometrySink, to: (f32, f32)| {
                    sink.AddArc(&D2D1_ARC_SEGMENT {
                        point: p(to.0, to.1),
                        size: D2D_SIZE_F { width: 1.7, height: 1.7 },
                        rotationAngle: 0.0,
                        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                        arcSize: D2D1_ARC_SIZE_LARGE,
                    })
                };
                sink.BeginFigure(p(3.0, 5.5), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddLine(p(5.8, 5.5));
                knob(&sink, (8.6, 5.5));
                sink.AddLine(p(11.5, 5.5));
                sink.AddLine(p(11.5, 7.8));
                knob(&sink, (11.5, 10.6));
                sink.AddLine(p(11.5, 13.5));
                sink.AddLine(p(3.0, 13.5));
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                let _ = sink.Close();
                rt.DrawGeometry(&path, brush, 1.4, &g.round);
            }
        },
        Icon::Search => unsafe {
            let e = D2D1_ELLIPSE { point: p(7.0, 7.0), radiusX: 4.5, radiusY: 4.5 };
            rt.DrawEllipse(&e, brush, w, &g.round);
            line((10.5, 10.5), (13.5, 13.5));
        },
        Icon::Globe => unsafe {
            let e = D2D1_ELLIPSE { point: p(8.0, 8.0), radiusX: 6.0, radiusY: 6.0 };
            rt.DrawEllipse(&e, brush, 1.3, &g.round);
            let e = D2D1_ELLIPSE { point: p(8.0, 8.0), radiusX: 2.6, radiusY: 6.0 };
            rt.DrawEllipse(&e, brush, 1.3, &g.round);
            line((2.0, 8.0), (14.0, 8.0));
        },
    }
}
