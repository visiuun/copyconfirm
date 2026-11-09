use crate::classify::{self, Kind};
use crate::config::{self, Config};
use crate::gfx::{Align, Canvas, Face, Gfx, Rect, color, glyph};
use crate::history::Entry;
use crate::imaging::Thumb;
use crate::sound::Player;
use crate::win::{self, Display, WM_APP_DOORBELL, WM_APP_OPEN, WM_MOUSELEAVE};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::{INFINITE, Sleep};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Result, w};

const BASE_HEIGHT: f32 = 40.0;
const BASE_GAP: f32 = 7.0;
const BASE_SHADOW: f32 = 12.0;
const BASE_ENTRY_TRAVEL: f32 = 24.0;
const SPRING_STIFFNESS: f32 = 380.0;
const SPRING_DAMPING: f32 = 32.0;
const HOVER_OPACITY: f32 = 0.10;
const MIN_FRAME: Duration = Duration::from_millis(6);

pub struct PillData {
    kind: Kind,
    text: Arc<str>,
    badge: Arc<str>,
    swatch: Option<[u8; 3]>,
    thumb: Option<Arc<Thumb>>,
    silent: bool,
}

impl PillData {
    pub fn from_entry(entry: &Entry, thumb: Option<Arc<Thumb>>) -> PillData {
        PillData {
            kind: entry.kind,
            text: entry.preview.clone(),
            badge: entry.badge.clone(),
            swatch: if entry.kind == Kind::Color { classify::parse_color(&entry.full) } else { None },
            thumb,
            silent: false,
        }
    }

    pub fn private() -> PillData {
        PillData { kind: Kind::Private, text: "Copied privately".into(), badge: "HIDDEN".into(), swatch: None, thumb: None, silent: false }
    }

    pub fn notice(text: &str, badge: &str) -> PillData {
        PillData { kind: Kind::Text, text: text.into(), badge: badge.into(), swatch: None, thumb: None, silent: true }
    }

    fn sample(kind: Kind, text: &str, badge: &str, swatch: Option<[u8; 3]>) -> PillData {
        PillData { kind, text: text.into(), badge: badge.into(), swatch, thumb: None, silent: false }
    }
}

pub enum Cmd {
    Show(PillData),
    Test,
    ConfigChanged,
    PlaySound,
}

static HWND_SLOT: AtomicIsize = AtomicIsize::new(0);
static SENDER: OnceLock<Sender<Cmd>> = OnceLock::new();
static HOST: AtomicIsize = AtomicIsize::new(0);

pub fn send(cmd: Cmd) {
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(cmd);
        win::post(HWND_SLOT.load(Ordering::Relaxed), WM_APP_DOORBELL);
    }
}

struct Sprite {
    w: i32,
    h: i32,
    pad: i32,
    pixels: Vec<u32>,
}

impl Sprite {
    fn core_h(&self) -> f32 {
        (self.h - 2 * self.pad) as f32
    }
}

struct Pill {
    data: PillData,
    sprite: Sprite,
    y: f32,
    velocity: f32,
    alpha: f32,
    leaving: bool,
    expires: Instant,
}

struct Surface {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl Surface {
    fn new(w: i32, h: i32) -> Option<Surface> {
        unsafe {
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let previous = SelectObject(dc, bitmap.into());
            Some(Surface { dc, bitmap, previous, bits: bits as *mut u32, w, h })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Overlay {
    hwnd: HWND,
    gfx: Rc<Gfx>,
    wic: IWICImagingFactory,
    player: Player,
    cfg: Config,
    pills: Vec<Pill>,
    queued: VecDeque<(Instant, PillData)>,
    display: Display,
    surface: Option<Surface>,
    zero_row: Vec<u32>,
    visible: bool,
    hovered: bool,
    tracking: bool,
    window_alpha: f32,
    origin: (i32, i32),
    last_frame: Instant,
    animating: bool,
}

thread_local! {
    static STATE: RefCell<Option<Overlay>> = const { RefCell::new(None) };
}

fn with_state<R>(f: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn truncate(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit.saturating_sub(1)) {
        Some((end, _)) if text.chars().count() > limit => format!("{}\u{2026}", &text[..end]),
        _ => text.to_string(),
    }
}

fn kind_glyph(kind: Kind) -> &'static str {
    match kind {
        Kind::Link => glyph::LINK,
        Kind::Email => glyph::MAIL,
        Kind::Code => glyph::CODE,
        Kind::Files => glyph::FOLDER,
        Kind::Image => glyph::PHOTO,
        Kind::Private => glyph::LOCK,
        _ => glyph::CHECK,
    }
}

impl Overlay {
    fn scale(&self) -> f32 {
        self.cfg.scale * self.display.dpi as f32 / 96.0
    }

    fn rasterize(&self, data: &PillData) -> Result<Sprite> {
        let s = self.scale();
        let cfg = &self.cfg;
        let font = self.display.dpi as f32 / 96.0 * cfg.font_scale * cfg.scale;
        let core_h = (BASE_HEIGHT * s).round();
        let pad = (BASE_SHADOW * s).ceil();
        let text = truncate(&data.text, cfg.max_preview_length);
        let badge = if cfg.show_char_count { &*data.badge } else { "" };
        let (text_size, badge_size) = (13.0 * font, 10.5 * font);

        let inset = 13.0 * s;
        let icon_box = (24.0 * s).round();
        let text_w = self.gfx.measure(&text, Face::UiBold, text_size).ceil();
        let badge_w = if badge.is_empty() { 0.0 } else { (self.gfx.measure(badge, Face::Ui, badge_size) + 16.0 * s).ceil() };
        let icon_w = if cfg.show_icon { icon_box + 10.0 * s } else { 0.0 };
        let badge_gap = if badge.is_empty() { 0.0 } else { 12.0 * s };
        let max_w = (self.display.work.right - self.display.work.left) as f32 * 0.85;
        let core_w = (inset * 2.0 + icon_w + text_w + badge_gap + badge_w).ceil().clamp((168.0 * s).min(max_w), max_w);
        let (w, h) = ((core_w + pad * 2.0) as u32, (core_h + pad * 2.0) as u32);

        unsafe {
            let bitmap = self.wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnDemand)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                ..Default::default()
            };
            let rt = self.gfx.d2d.CreateWicBitmapRenderTarget(&bitmap, &props)?;
            let c = Canvas::new(rt.clone(), self.gfx.clone())?;
            c.rt.BeginDraw();
            c.rt.Clear(Some(&color(0, 0.0)));

            let core = Rect::new(pad, pad, core_w, core_h);
            let radius = core_h / 2.0;
            // Soft shadow from stacked translucent shells.
            let layers = 7;
            for i in 0..layers {
                let spread = pad * (layers - i) as f32 / layers as f32 - 1.0;
                let shell = Rect::new(core.x - spread, core.y - spread + 2.0 * s, core.w + 2.0 * spread, core.h + 2.0 * spread - 2.0 * s);
                c.fill_rr(shell, radius + spread, 0x000000, 0.055);
            }
            c.fill_rr(core, radius, 0x080808, 1.0);
            c.stroke_rr(core.inset(0.5), radius - 0.5, 0x2C2C2C, 1.0, 1.0);

            let mid = core.y + core_h / 2.0;
            let mut x = core.x + inset;
            if cfg.show_icon {
                let tile = Rect::new(x, mid - icon_box / 2.0, icon_box, icon_box);
                self.draw_tile(&c, data, tile);
                x += icon_box + 10.0 * s;
            }
            let right = core.x + core_w - inset;
            if !badge.is_empty() {
                let bh = (20.0 * s).round();
                let chip = Rect::new(right - badge_w, mid - bh / 2.0, badge_w, bh);
                c.fill_rr(chip, bh / 2.0, 0x161616, 1.0);
                c.stroke_rr(chip.inset(0.5), bh / 2.0 - 0.5, 0x303030, 1.0, 1.0);
                c.text(badge, Face::Ui, badge_size, chip, 0xB0B0B0, Align::Center);
            }
            let text_right = right - if badge.is_empty() { 0.0 } else { badge_w + badge_gap };
            c.text(&text, Face::UiBold, text_size, Rect::new(x, core.y, text_right - x, core_h), 0xFFFFFF, Align::Left);
            c.rt.EndDraw(None, None)?;
            drop(c);
            drop(rt);

            let mut pixels = vec![0u32; (w * h) as usize];
            let bytes = std::slice::from_raw_parts_mut(pixels.as_mut_ptr() as *mut u8, pixels.len() * 4);
            bitmap.CopyPixels(std::ptr::null(), w * 4, bytes)?;
            Ok(Sprite { w: w as i32, h: h as i32, pad: pad as i32, pixels })
        }
    }

    fn draw_tile(&self, c: &Canvas, data: &PillData, tile: Rect) {
        let s = self.scale();
        let radius = 7.0 * s;
        if let (Some([r, g, b]), true) = (data.swatch, self.cfg.detect_colors) {
            let rgb = (r as u32) << 16 | (g as u32) << 8 | b as u32;
            c.fill_rr(tile, radius, rgb, 1.0);
            c.stroke_rr(tile.inset(0.5), radius - 0.5, 0xFFFFFF, 0.55, 1.0);
            return;
        }
        if let (Some(thumb), true) = (&data.thumb, self.cfg.detect_images)
            && let Some(bitmap) = c.create_bitmap(thumb)
        {
            c.bitmap_rr(&bitmap, tile, radius);
            c.stroke_rr(tile.inset(0.5), radius - 0.5, 0xFFFFFF, 0.28, 1.0);
            return;
        }
        c.fill_circle(tile.x + tile.w / 2.0, tile.y + tile.h / 2.0, tile.w / 2.0, 0x1C1C1C, 1.0);
        c.icon(kind_glyph(data.kind), 12.0 * s, tile, 0xFFFFFF);
    }

    fn slot_height(&self) -> (f32, f32) {
        let s = self.scale();
        ((BASE_HEIGHT * s).round(), (BASE_GAP * s).round())
    }

    fn add_pill(&mut self, data: PillData) {
        if self.pills.is_empty() {
            self.display = win::active_display(self.cfg.follow_cursor);
        }
        if !data.silent && !self.cfg.paused {
            self.player.play(self.cfg.sound, self.cfg.sound_volume);
        }
        let Ok(sprite) = self.rasterize(&data) else { return };
        let travel = BASE_ENTRY_TRAVEL * self.scale();
        let expires = Instant::now() + Duration::from_secs_f32(self.cfg.popup_duration);
        self.pills.insert(0, Pill { data, sprite, y: -travel, velocity: 0.0, alpha: 0.0, leaving: false, expires });
        let limit = self.cfg.stack_limit;
        for pill in self.pills.iter_mut().filter(|p| !p.leaving).skip(limit) {
            pill.leaving = true;
        }
        self.window_alpha = self.window_alpha.max(self.cfg.opacity * 0.6);
        self.animating = true;
        if !self.visible {
            self.window_alpha = self.cfg.opacity;
            self.show_window();
        }
    }

    fn show_window(&mut self) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        self.visible = true;
    }

    fn hide_window(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.visible = false;
        self.hovered = false;
        self.tracking = false;
        self.surface = None;
        self.zero_row = Vec::new();
    }

    fn apply_click_through(&self) {
        unsafe {
            let style = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            let flag = WS_EX_TRANSPARENT.0;
            let next = if self.cfg.click_through { style | flag } else { style & !flag };
            if next != style {
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, next as isize);
            }
        }
    }

    fn apply_config(&mut self) {
        let before = self.cfg;
        self.cfg = config::get();
        self.apply_click_through();
        let looks_changed = before.scale != self.cfg.scale
            || before.font_scale != self.cfg.font_scale
            || before.show_icon != self.cfg.show_icon
            || before.show_char_count != self.cfg.show_char_count
            || before.max_preview_length != self.cfg.max_preview_length
            || before.detect_colors != self.cfg.detect_colors
            || before.detect_images != self.cfg.detect_images;
        if looks_changed {
            let sprites: Vec<_> = self.pills.iter().map(|p| self.rasterize(&p.data)).collect();
            for (pill, sprite) in self.pills.iter_mut().zip(sprites) {
                if let Ok(sprite) = sprite {
                    pill.sprite = sprite;
                }
            }
        }
        self.animating = !self.pills.is_empty();
    }

    fn drain(&mut self, rx: &Receiver<Cmd>) {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Show(data) => self.add_pill(data),
                Cmd::ConfigChanged => self.apply_config(),
                Cmd::PlaySound => self.player.play(self.cfg.sound, self.cfg.sound_volume),
                Cmd::Test => {
                    let now = Instant::now();
                    let samples = [
                        PillData::sample(Kind::Link, "https://example.com/welcome", "LINK", None),
                        PillData::sample(Kind::Color, "#1DB954", "COLOR", Some([0x1D, 0xB9, 0x54])),
                        PillData::sample(Kind::Code, "const client = new Client({ intents });", "39c code", None),
                    ];
                    for (i, sample) in samples.into_iter().enumerate() {
                        self.queued.push_back((now + Duration::from_millis(120 * i as u64), sample));
                    }
                }
            }
        }
    }

    fn wait_ms(&self) -> u32 {
        if self.animating {
            return 0;
        }
        let now = Instant::now();
        let next_expiry = self.pills.iter().filter(|p| !p.leaving).map(|p| p.expires).min();
        let next_queued = self.queued.front().map(|q| q.0);
        match next_expiry.into_iter().chain(next_queued).min() {
            Some(at) => (at.saturating_duration_since(now).as_millis() as u32).saturating_add(1),
            None => INFINITE,
        }
    }

    fn pump_timers(&mut self) {
        let now = Instant::now();
        while self.queued.front().is_some_and(|q| q.0 <= now) {
            let (_, data) = self.queued.pop_front().unwrap();
            self.add_pill(data);
        }
        for pill in self.pills.iter_mut().filter(|p| !p.leaving) {
            if pill.expires <= now {
                pill.leaving = true;
                self.animating = true;
            }
        }
    }

    fn step(&mut self, dt: f32) {
        let (_, gap) = self.slot_height();
        let ease = |k: f32| 1.0 - (-dt * k).exp();
        let mut offset = 0.0f32;
        let mut moving = false;
        for pill in self.pills.iter_mut() {
            let target = offset;
            if !pill.leaving {
                offset += pill.sprite.core_h() + gap;
            }
            if pill.leaving {
                pill.alpha -= pill.alpha.max(0.12) * ease(16.0);
                moving = true;
            } else {
                let delta = 1.0 - pill.alpha;
                if delta > 0.004 {
                    pill.alpha += delta * ease(20.0);
                    moving = true;
                } else {
                    pill.alpha = 1.0;
                }
                let mut left = dt;
                while left > 0.0 {
                    let h = left.min(1.0 / 240.0);
                    pill.velocity += (SPRING_STIFFNESS * (target - pill.y) - SPRING_DAMPING * pill.velocity) * h;
                    pill.y += pill.velocity * h;
                    left -= h;
                }
                if (pill.y - target).abs() < 0.05 && pill.velocity.abs() < 2.0 {
                    pill.y = target;
                    pill.velocity = 0.0;
                } else {
                    moving = true;
                }
            }
        }
        self.pills.retain(|p| !(p.leaving && p.alpha <= 0.015));
        let target_alpha = if self.hovered { HOVER_OPACITY } else { self.cfg.opacity };
        let delta = target_alpha - self.window_alpha;
        if delta.abs() > 0.004 {
            self.window_alpha += delta * ease(if self.hovered { 30.0 } else { 18.0 });
            moving = true;
        } else {
            self.window_alpha = target_alpha;
        }
        self.animating = moving;
    }

    fn window_extent(&self) -> (i32, i32) {
        let (h, gap) = self.slot_height();
        let pad = (BASE_SHADOW.max(BASE_ENTRY_TRAVEL) * self.scale()).ceil() as i32;
        let w = self.pills.iter().map(|p| p.sprite.w).max().unwrap_or(1);
        let height = ((self.cfg.stack_limit + 1) as f32 * (h + gap)).ceil() as i32 + 2 * pad;
        (w, height)
    }

    fn compose(&mut self) {
        let (w, h) = self.window_extent();
        if self.surface.as_ref().is_none_or(|s| s.w < w || s.h < h) {
            self.surface = Surface::new(w, h);
            self.zero_row = vec![0; w as usize];
        }
        let Some(surface) = &self.surface else { return };
        let stride = surface.w as usize;
        let rows = surface.h as usize;
        let target = unsafe { std::slice::from_raw_parts_mut(surface.bits, stride * rows) };
        target.fill(0);

        let pad = (BASE_SHADOW.max(BASE_ENTRY_TRAVEL) * self.scale()).ceil();
        let anchor = self.cfg.position;
        for pill in self.pills.iter().rev() {
            let sprite = &pill.sprite;
            let top = if anchor.is_bottom() {
                h as f32 - pad - sprite.core_h() - pill.y - sprite.pad as f32
            } else {
                pad + pill.y - sprite.pad as f32
            };
            let x = match anchor.column() {
                0 => 0,
                1 => (w - sprite.w) / 2,
                _ => w - sprite.w,
            };
            blit(target, stride, rows, x as usize, top, sprite, (pill.alpha * 256.0) as u32, &self.zero_row);
        }

        let margin = (self.cfg.screen_margin as f32 * self.display.dpi as f32 / 96.0) as i32;
        let work = self.display.work;
        let shadow = (BASE_SHADOW * self.scale()).ceil() as i32;
        let edge_pad = pad as i32;
        let wx = match anchor.column() {
            0 => work.left + margin - shadow,
            1 => work.left + (work.right - work.left - w) / 2,
            _ => work.right - margin - w + shadow,
        };
        let wy = if anchor.is_bottom() { work.bottom - margin - h + edge_pad } else { work.top + margin - edge_pad };
        self.origin = (wx, wy);

        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (self.window_alpha.clamp(0.0, 1.0) * 255.0) as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&POINT { x: wx, y: wy }),
                Some(&SIZE { cx: w, cy: h }),
                Some(surface.dc),
                Some(&POINT { x: 0, y: 0 }),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }

    fn frame(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.step(dt);
        if self.pills.is_empty() {
            if self.visible {
                self.hide_window();
            }
            self.animating = false;
            return;
        }
        self.compose();
    }

    fn pill_at(&self, x: i32, y: i32) -> Option<usize> {
        let (w, h) = self.window_extent();
        let pad = (BASE_SHADOW.max(BASE_ENTRY_TRAVEL) * self.scale()).ceil() as i32;
        let anchor = self.cfg.position;
        self.pills.iter().position(|pill| {
            let sprite = &pill.sprite;
            let core_h = sprite.core_h() as i32;
            let top = if anchor.is_bottom() { h - pad - core_h - pill.y as i32 } else { pad + pill.y as i32 };
            let left = match anchor.column() {
                0 => 0,
                1 => (w - sprite.w) / 2,
                _ => w - sprite.w,
            } + sprite.pad;
            !pill.leaving && x >= left && x < left + sprite.w - 2 * sprite.pad && y >= top && y < top + core_h
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn blit(target: &mut [u32], stride: usize, rows: usize, x: usize, top: f32, sprite: &Sprite, alpha: u32, zero: &[u32]) {
    let w = sprite.w as usize;
    let h = sprite.h as usize;
    let whole = top.floor();
    let frac = ((top - whole) * 256.0) as u32;
    let whole = whole as i32;
    let source_row = |index: i32| -> &[u32] {
        if index >= 0 && (index as usize) < h { &sprite.pixels[index as usize * w..(index as usize + 1) * w] } else { &zero[..w] }
    };
    for row in whole.max(0)..(whole + h as i32 + 1).min(rows as i32) {
        let sy = row - whole;
        let (upper, lower) = (source_row(sy - 1), source_row(sy));
        let start = row as usize * stride + x;
        let line = &mut target[start..start + w];
        for ((dst, &a), &b) in line.iter_mut().zip(upper).zip(lower) {
            if a | b == 0 {
                continue;
            }
            let keep = 256 - frac;
            let rb = ((a & 0x00FF_00FF) * frac + (b & 0x00FF_00FF) * keep) >> 8 & 0x00FF_00FF;
            let ag = ((a >> 8 & 0x00FF_00FF) * frac + (b >> 8 & 0x00FF_00FF) * keep) >> 8 & 0x00FF_00FF;
            let rb = (rb * alpha) >> 8 & 0x00FF_00FF;
            let ag = (ag * alpha) >> 8 & 0x00FF_00FF;
            let inverse = 256 - (ag >> 16);
            let d = *dst;
            let d_rb = ((d & 0x00FF_00FF) * inverse) >> 8 & 0x00FF_00FF;
            let d_ag = ((d >> 8 & 0x00FF_00FF) * inverse) >> 8 & 0x00FF_00FF;
            *dst = (rb + d_rb) | ((ag + d_ag) << 8);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                with_state(|o| {
                    if !o.tracking {
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut track);
                        o.tracking = true;
                    }
                    if !o.hovered {
                        o.hovered = true;
                        o.animating = true;
                    }
                });
                return LRESULT(0);
            }
            WM_MOUSELEAVE => {
                with_state(|o| {
                    o.tracking = false;
                    o.hovered = false;
                    o.animating = true;
                });
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, (lp.0 >> 16 & 0xFFFF) as i16 as i32);
                with_state(|o| {
                    if let Some(index) = o.pill_at(x, y) {
                        o.pills[index].leaving = true;
                        o.animating = true;
                    }
                });
                return LRESULT(0);
            }
            WM_RBUTTONUP => {
                win::post(HOST.load(Ordering::Relaxed), WM_APP_OPEN);
                return LRESULT(0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

fn create_state(hwnd: HWND) -> Result<Overlay> {
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        Ok(Overlay {
            hwnd,
            gfx: Gfx::new()?,
            wic,
            player: Player::new(),
            cfg: config::get(),
            pills: Vec::new(),
            queued: VecDeque::new(),
            display: win::active_display(true),
            surface: None,
            zero_row: Vec::new(),
            visible: false,
            hovered: false,
            tracking: false,
            window_alpha: 0.0,
            origin: (0, 0),
            last_frame: Instant::now(),
            animating: false,
        })
    }
}

pub fn spawn(host: isize) {
    HOST.store(host, Ordering::Relaxed);
    let (tx, rx) = channel();
    let _ = SENDER.set(tx);
    let (ready_tx, ready_rx) = channel();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let class = w!("CopyHUD.Overlay");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: win::instance(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let ex = WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let Ok(hwnd) = CreateWindowExW(ex, class, w!("CopyHUD Overlay"), WS_POPUP, 0, 0, 1, 1, None, None, Some(win::instance()), None)
        else {
            let _ = ready_tx.send(());
            return;
        };
        HWND_SLOT.store(hwnd.0 as isize, Ordering::Relaxed);
        let Ok(state) = create_state(hwnd) else {
            let _ = ready_tx.send(());
            return;
        };
        state.apply_click_through();
        STATE.with(|s| *s.borrow_mut() = Some(state));
        let _ = ready_tx.send(());

        let mut msg = MSG::default();
        loop {
            let wait = with_state(|o| o.wait_ms()).unwrap_or(INFINITE);
            if wait != 0 {
                MsgWaitForMultipleObjectsEx(None, wait, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_QUIT {
                    return;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let animating = with_state(|o| {
                let was_idle = !o.animating;
                o.drain(&rx);
                o.pump_timers();
                if o.animating {
                    if was_idle {
                        o.last_frame = Instant::now() - Duration::from_millis(8);
                    }
                    o.frame();
                }
                o.animating
            })
            .unwrap_or(false);
            if animating {
                // DwmFlush can return early with nothing queued, so cap the rate here.
                let started = Instant::now();
                let _ = DwmFlush();
                if let Some(rest) = MIN_FRAME.checked_sub(started.elapsed()) {
                    Sleep(rest.as_millis() as u32 + 1);
                }
            }
        }
    });
    let _ = ready_rx.recv();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite(pixel: u32) -> Sprite {
        Sprite { w: 2, h: 2, pad: 0, pixels: vec![pixel; 4] }
    }

    #[test]
    fn opaque_blit_replaces_background() {
        let mut target = vec![0xFF10_2030u32; 4];
        blit(&mut target, 2, 2, 0, 0.0, &sprite(0xFFFF_FFFF), 256, &[0; 2]);
        assert!(target.iter().all(|&p| p >> 24 >= 0xFE && p & 0xFF >= 0xFE), "{target:x?}");
    }

    #[test]
    fn transparent_sprite_leaves_target_untouched() {
        let mut target = vec![0x8040_2010u32; 4];
        blit(&mut target, 2, 2, 0, 0.0, &sprite(0), 256, &[0; 2]);
        assert!(target.iter().all(|&p| p == 0x8040_2010));
    }

    #[test]
    fn half_alpha_halves_coverage() {
        let mut target = vec![0u32; 4];
        blit(&mut target, 2, 2, 0, 0.0, &sprite(0xFFFF_FFFF), 128, &[0; 2]);
        let a = target[0] >> 24;
        assert!((120..=135).contains(&a), "{a}");
    }

    #[test]
    fn offscreen_rows_are_clipped() {
        let mut target = vec![0u32; 4];
        blit(&mut target, 2, 2, 0, -40.5, &sprite(0xFFFF_FFFF), 256, &[0; 2]);
        blit(&mut target, 2, 2, 0, 40.5, &sprite(0xFFFF_FFFF), 256, &[0; 2]);
        assert!(target.iter().all(|&p| p == 0));
    }

    #[test]
    fn truncation_counts_characters() {
        assert_eq!(truncate("héllo", 10), "héllo");
        assert_eq!(truncate("héllo wörld", 5), "héll\u{2026}");
    }
}
