use crate::imaging::Thumb;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::core::{BOOL, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

pub mod glyph {
    pub const CHECK: &str = "\u{E73E}";
    pub const LINK: &str = "\u{E71B}";
    pub const MAIL: &str = "\u{E715}";
    pub const CODE: &str = "\u{E943}";
    pub const PAGE: &str = "\u{E8A5}";
    pub const FOLDER: &str = "\u{E8B7}";
    pub const PHOTO: &str = "\u{EB9F}";
    pub const LOCK: &str = "\u{E72E}";
    pub const SEARCH: &str = "\u{E721}";
    pub const CLOSE: &str = "\u{E711}";
    pub const CLIP: &str = "\u{E8C8}";
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
pub enum Face {
    Ui,
    UiBold,
    Mono,
    Icon,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

pub fn color(rgb: u32, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: ((rgb >> 16) & 255) as f32 / 255.0, g: ((rgb >> 8) & 255) as f32 / 255.0, b: (rgb & 255) as f32 / 255.0, a: alpha }
}

#[derive(Clone, Copy, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        Rect::new(x, y, (self.x + self.w).min(o.x + o.w) - x, (self.y + self.h).min(o.y + o.h) - y)
    }

    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }

    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F { left: self.x, top: self.y, right: self.x + self.w, bottom: self.y + self.h }
    }
}

pub struct Gfx {
    pub d2d: ID2D1Factory,
    pub dw: IDWriteFactory,
    ui: Vec<u16>,
    mono: Vec<u16>,
    icon: Vec<u16>,
    formats: RefCell<HashMap<(Face, u32), IDWriteTextFormat>>,
}

fn pick_family(dw: &IDWriteFactory, names: &[PCWSTR]) -> Vec<u16> {
    unsafe {
        let mut collection = None;
        let _ = dw.GetSystemFontCollection(&mut collection, false);
        if let Some(collection) = collection {
            for name in names {
                let (mut index, mut found) = (0, BOOL(0));
                if collection.FindFamilyName(*name, &mut index, &mut found).is_ok() && found.as_bool() {
                    return name.as_wide().iter().copied().chain([0]).collect();
                }
            }
        }
        names[names.len() - 1].as_wide().iter().copied().chain([0]).collect()
    }
}

impl Gfx {
    pub fn new() -> Result<Rc<Gfx>> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dw: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let ui = pick_family(&dw, &[w!("Segoe UI Variable Text"), w!("Segoe UI")]);
            let mono = pick_family(&dw, &[w!("Cascadia Mono"), w!("Consolas")]);
            let icon = pick_family(&dw, &[w!("Segoe Fluent Icons"), w!("Segoe MDL2 Assets")]);
            Ok(Rc::new(Gfx { d2d, dw, ui, mono, icon, formats: RefCell::new(HashMap::new()) }))
        }
    }

    fn format(&self, face: Face, size: f32) -> Result<IDWriteTextFormat> {
        let key = (face, (size * 8.0).round() as u32);
        if let Some(found) = self.formats.borrow().get(&key) {
            return Ok(found.clone());
        }
        unsafe {
            let family = match face {
                Face::Ui | Face::UiBold => &self.ui,
                Face::Mono => &self.mono,
                Face::Icon => &self.icon,
            };
            let weight = if face == Face::UiBold { DWRITE_FONT_WEIGHT_SEMI_BOLD } else { DWRITE_FONT_WEIGHT_NORMAL };
            let format = self.dw.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            let ellipsis = self.dw.CreateEllipsisTrimmingSign(&format)?;
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
            format.SetTrimming(&trimming, &ellipsis)?;
            self.formats.borrow_mut().insert(key, format.clone());
            Ok(format)
        }
    }

    fn layout(&self, text: &str, face: Face, size: f32, w: f32, h: f32) -> Result<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe { self.dw.CreateTextLayout(&wide, &self.format(face, size)?, w.max(1.0), h.max(1.0)) }
    }

    pub fn measure(&self, text: &str, face: Face, size: f32) -> f32 {
        unsafe {
            self.layout(text, face, size, 16384.0, 256.0)
                .and_then(|l| {
                    let mut metrics = DWRITE_TEXT_METRICS::default();
                    l.GetMetrics(&mut metrics)?;
                    Ok(metrics.widthIncludingTrailingWhitespace)
                })
                .unwrap_or(0.0)
        }
    }
}

pub struct Canvas {
    pub rt: ID2D1RenderTarget,
    brush: ID2D1SolidColorBrush,
    pub gfx: Rc<Gfx>,
}

fn rounded(r: Rect, radius: f32) -> D2D1_ROUNDED_RECT {
    D2D1_ROUNDED_RECT { rect: r.d2d(), radiusX: radius, radiusY: radius }
}

impl Canvas {
    pub fn new(rt: ID2D1RenderTarget, gfx: Rc<Gfx>) -> Result<Canvas> {
        unsafe {
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            let brush = rt.CreateSolidColorBrush(&color(0, 1.0), None)?;
            Ok(Canvas { rt, brush, gfx })
        }
    }

    fn paint(&self, rgb: u32, alpha: f32) -> &ID2D1SolidColorBrush {
        unsafe { self.brush.SetColor(&color(rgb, alpha)) };
        &self.brush
    }

    pub fn fill_rr(&self, r: Rect, radius: f32, rgb: u32, alpha: f32) {
        unsafe { self.rt.FillRoundedRectangle(&rounded(r, radius), self.paint(rgb, alpha)) };
    }

    pub fn stroke_rr(&self, r: Rect, radius: f32, rgb: u32, alpha: f32, width: f32) {
        unsafe { self.rt.DrawRoundedRectangle(&rounded(r, radius), self.paint(rgb, alpha), width, None) };
    }

    pub fn fill(&self, r: Rect, rgb: u32, alpha: f32) {
        unsafe { self.rt.FillRectangle(&r.d2d(), self.paint(rgb, alpha)) };
    }

    pub fn fill_circle(&self, cx: f32, cy: f32, radius: f32, rgb: u32, alpha: f32) {
        let ellipse = D2D1_ELLIPSE { point: Vector2 { X: cx, Y: cy }, radiusX: radius, radiusY: radius };
        unsafe { self.rt.FillEllipse(&ellipse, self.paint(rgb, alpha)) };
    }

    pub fn text(&self, s: &str, face: Face, size: f32, r: Rect, rgb: u32, align: Align) {
        let Ok(layout) = self.gfx.layout(s, face, size, r.w, r.h) else { return };
        unsafe {
            let alignment = match align {
                Align::Left => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::Right => DWRITE_TEXT_ALIGNMENT_TRAILING,
            };
            let _ = layout.SetTextAlignment(alignment);
            self.rt.DrawTextLayout(
                Vector2 { X: r.x, Y: r.y },
                &layout,
                self.paint(rgb, 1.0),
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );
        }
    }

    pub fn icon(&self, s: &str, size: f32, r: Rect, rgb: u32) {
        self.text(s, Face::Icon, size, r, rgb, Align::Center);
    }

    pub fn create_bitmap(&self, thumb: &Thumb) -> Option<ID2D1Bitmap> {
        unsafe {
            let props = D2D1_BITMAP_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
            };
            self.rt
                .CreateBitmap(
                    D2D_SIZE_U { width: thumb.width, height: thumb.height },
                    Some(thumb.pixels.as_ptr() as *const _),
                    thumb.width * 4,
                    &props,
                )
                .ok()
        }
    }

    pub fn bitmap_rr(&self, bitmap: &ID2D1Bitmap, r: Rect, radius: f32) {
        unsafe {
            let size = bitmap.GetPixelSize();
            let (sx, sy) = (r.w / size.width.max(1) as f32, r.h / size.height.max(1) as f32);
            let brush_props =
                D2D1_BRUSH_PROPERTIES { opacity: 1.0, transform: Matrix3x2 { M11: sx, M12: 0.0, M21: 0.0, M22: sy, M31: r.x, M32: r.y } };
            let bitmap_props = D2D1_BITMAP_BRUSH_PROPERTIES {
                extendModeX: D2D1_EXTEND_MODE_CLAMP,
                extendModeY: D2D1_EXTEND_MODE_CLAMP,
                interpolationMode: D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
            };
            if let Ok(brush) = self.rt.CreateBitmapBrush(bitmap, Some(&bitmap_props), Some(&brush_props)) {
                self.rt.FillRoundedRectangle(&rounded(r, radius), &brush);
            }
        }
    }

    pub fn push_clip(&self, r: Rect) {
        unsafe { self.rt.PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn pop_clip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() };
    }
}
