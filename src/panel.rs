use crate::classify::{Kind, contains_ci};
use crate::config::{self, Anchor, Config};
use crate::gfx::{Align, Canvas, Face, Gfx, Rect, color, glyph};
use crate::history::{self, Entry, History};
use crate::install::{autostart_enabled, set_autostart};
use crate::overlay::{self, Cmd, PillData};
use crate::sound;
use crate::win::{self, WM_APP_KEY, WM_MOUSELEAVE};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{EM_SETCUEBANNER, SetWindowTheme};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, Result, w};

pub const CLASS: PCWSTR = w!("CopyHUD.Panel");

const BG: u32 = 0x000000;
const FIELD: u32 = 0x0E0E0E;
const HOVER: u32 = 0x101010;
const SELECTED: u32 = 0x181818;
const LINE: u32 = 0x1E1E1E;
const TRACK: u32 = 0x262626;
const TEXT: u32 = 0xFFFFFF;
const TEXT2: u32 = 0xA0A0A0;
const TEXT3: u32 = 0x767676;

const MARGIN: f32 = 16.0;
const HEADER: f32 = 58.0;
const ROW: f32 = 56.0;
const ROW_GAP: f32 = 2.0;
const FOOTER: f32 = 38.0;
const DEFAULT_SIZE: (i32, i32) = (460, 680);
const MIN_SIZE: (i32, i32) = (420, 520);

const FILTERS: [(&str, Option<&[Kind]>); 7] = [
    ("All", None),
    ("Text", Some(&[Kind::Text, Kind::Email])),
    ("Links", Some(&[Kind::Link])),
    ("Code", Some(&[Kind::Code])),
    ("Images", Some(&[Kind::Image])),
    ("Colors", Some(&[Kind::Color])),
    ("Files", Some(&[Kind::Files])),
];

mod id {
    pub const TAB: u32 = 10;
    pub const SCROLLBAR: u32 = 20;
    pub const CLEAR: u32 = 21;
    pub const TEST: u32 = 22;
    pub const RESET: u32 = 23;
    pub const QUIT: u32 = 24;
    pub const CREDIT: u32 = 25;
    pub const FILTER: u32 = 30;
    pub const ANCHOR: u32 = 40;
    pub const SOUND: u32 = 50;
    pub const SLIDER: u32 = 70;
    pub const SWITCH: u32 = 90;
    pub const ROW: u32 = 1000;
    pub const DELETE: u32 = 2000;
    pub const ANIM_TAB: u32 = 3000;
}

pub enum Action {
    Copy(usize, bool),
    Delete(usize),
    Clear,
    Quit,
}

#[derive(Default)]
pub struct Output {
    pub actions: Vec<Action>,
    pub config_changed: bool,
    pub history_policy_changed: bool,
}

#[derive(Clone, Copy, Default)]
struct Scroll {
    pos: f32,
    target: f32,
    max: f32,
}

#[derive(Clone, Copy, Default)]
struct Hit {
    hot: bool,
    active: bool,
    clicked: bool,
    released: bool,
}

static EDIT_BRUSH: AtomicIsize = AtomicIsize::new(0);

pub struct Panel {
    pub hwnd: HWND,
    edit: HWND,
    edit_font: HFONT,
    hrt: ID2D1HwndRenderTarget,
    c: Canvas,
    dpi: u32,
    size: (f32, f32),

    tab: usize,
    scroll: [Scroll; 2],
    view: Vec<usize>,
    view_dirty: bool,
    query: String,
    filter: usize,
    selected: usize,
    list_height: f32,

    mouse: (f32, f32),
    hot: u32,
    active: u32,
    clicked: u32,
    released: u32,
    hits: Vec<(Rect, u32)>,
    clip: Rect,

    anims: HashMap<u32, f32>,
    animating: bool,
    timer_on: bool,
    last_paint: Instant,
    dt: f32,

    thumbs: HashMap<Arc<str>, Option<ID2D1Bitmap>>,
    flash: Option<(Arc<str>, Instant)>,
    confirm_clear: Option<Instant>,
    autostart: bool,
    pub hotkey_ok: bool,
    output: Output,
}

fn dips(px: i32, dpi: u32) -> f32 {
    px as f32 * 96.0 / dpi as f32
}

fn px(dip: f32, dpi: u32) -> i32 {
    (dip * dpi as f32 / 96.0).round() as i32
}

fn relative_time(timestamp: f64) -> String {
    let age = (history::now() - timestamp).max(0.0) as u64;
    match age {
        0..=44 => "now".into(),
        45..=3599 => format!("{}m ago", (age / 60).max(1)),
        3600..=86399 => format!("{}h ago", age / 3600),
        _ => format!("{}d ago", age / 86400),
    }
}

fn row_glyph(kind: Kind) -> &'static str {
    match kind {
        Kind::Link => glyph::LINK,
        Kind::Email => glyph::MAIL,
        Kind::Code => glyph::CODE,
        Kind::Files => glyph::FOLDER,
        Kind::Image => glyph::PHOTO,
        Kind::Private => glyph::LOCK,
        _ => glyph::PAGE,
    }
}

unsafe extern "system" fn edit_subclass(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _: usize, _: usize) -> LRESULT {
    unsafe {
        let vk = wp.0 as u16;
        match msg {
            WM_KEYDOWN if vk == VK_UP.0 || vk == VK_DOWN.0 || vk == VK_RETURN.0 || vk == VK_ESCAPE.0 => {
                if let Ok(parent) = GetParent(hwnd) {
                    let _ = PostMessageW(Some(parent), WM_APP_KEY, wp, LPARAM(0));
                }
                return LRESULT(0);
            }
            // Swallowed so the edit control doesn't beep.
            WM_CHAR if vk == 13 || vk == 27 => return LRESULT(0),
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

unsafe fn create_edit_font(dpi: u32) -> HFONT {
    unsafe {
        CreateFontW(
            -px(13.0, dpi),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            ANTIALIASED_QUALITY,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Segoe UI"),
        )
    }
}

fn create_target(gfx: &Gfx, hwnd: HWND, dpi: u32) -> Result<ID2D1HwndRenderTarget> {
    unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(hwnd, &mut rc);
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_UNKNOWN, alphaMode: D2D1_ALPHA_MODE_IGNORE },
            dpiX: dpi as f32,
            dpiY: dpi as f32,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: D2D_SIZE_U { width: rc.right.max(1) as u32, height: rc.bottom.max(1) as u32 },
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        gfx.d2d.CreateHwndRenderTarget(&props, &hwnd_props)
    }
}

impl Panel {
    pub fn open(gfx: Rc<Gfx>, tab: usize, hotkey_ok: bool) -> Result<Box<Panel>> {
        unsafe {
            let display = win::active_display(true);
            let dpi = display.dpi;
            let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
            let mut frame = RECT { left: 0, top: 0, right: px(DEFAULT_SIZE.0 as f32, dpi), bottom: px(DEFAULT_SIZE.1 as f32, dpi) };
            let _ = AdjustWindowRectExForDpi(&mut frame, style, false, WINDOW_EX_STYLE(0), dpi);
            let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
            let work = display.work;
            let (x, y) = (work.left + (work.right - work.left - w) / 2, work.top + (work.bottom - work.top - h) / 2);
            let hwnd =
                CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, w!("Copy HUD"), style, x, y, w, h, None, None, Some(win::instance()), None)?;

            let dark: BOOL = true.into();
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as *const _, 4);
            let caption = COLORREF(0);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &caption as *const _ as *const _, 4);
            let border = COLORREF(0x00222222);
            let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);

            if EDIT_BRUSH.load(Ordering::Relaxed) == 0 {
                EDIT_BRUSH.store(CreateSolidBrush(COLORREF(FIELD)).0 as isize, Ordering::Relaxed);
            }
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                10,
                10,
                Some(hwnd),
                None,
                Some(win::instance()),
                None,
            )?;
            let edit_font = create_edit_font(dpi);
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(edit_font.0 as usize)), Some(LPARAM(1)));
            let _ = SetWindowTheme(edit, w!("DarkMode_CFD"), PCWSTR::null());
            let cue = w!("Search clipboard history");
            SendMessageW(edit, EM_SETCUEBANNER, Some(WPARAM(1)), Some(LPARAM(cue.as_ptr() as isize)));
            let _ = SetWindowSubclass(edit, Some(edit_subclass), 1, 0);

            let hrt = create_target(&gfx, hwnd, dpi)?;
            let rt: ID2D1RenderTarget = windows::core::Interface::cast(&hrt)?;
            let c = Canvas::new(rt, gfx)?;

            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let mut panel = Box::new(Panel {
                hwnd,
                edit,
                edit_font,
                hrt,
                c,
                dpi,
                size: (dips(rc.right, dpi), dips(rc.bottom, dpi)),
                tab,
                scroll: [Scroll::default(); 2],
                view: Vec::new(),
                view_dirty: true,
                query: String::new(),
                filter: 0,
                selected: 0,
                list_height: 0.0,
                mouse: (-1.0, -1.0),
                hot: 0,
                active: 0,
                clicked: 0,
                released: 0,
                hits: Vec::with_capacity(64),
                clip: Rect::default(),
                anims: HashMap::new(),
                animating: false,
                timer_on: false,
                last_paint: Instant::now(),
                dt: 0.0,
                thumbs: HashMap::new(),
                flash: None,
                confirm_clear: None,
                autostart: autostart_enabled(),
                hotkey_ok,
                output: Output::default(),
            });
            panel.layout_edit();
            panel.anims.insert(id::ANIM_TAB, tab as f32);
            Ok(panel)
        }
    }

    pub fn present(&mut self, store: &History, activate: bool) -> Output {
        let output = self.paint(store);
        unsafe {
            let _ = ShowWindow(self.hwnd, if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
            if activate {
                self.focus();
            }
        }
        output
    }

    pub fn focus(&self) {
        unsafe {
            let _ = SetForegroundWindow(self.hwnd);
            let _ = SetFocus(Some(if self.tab == 0 { self.edit } else { self.hwnd }));
        }
    }

    pub fn is_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd }
    }

    pub fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn history_changed(&mut self) {
        self.view_dirty = true;
        self.invalidate();
    }

    fn layout_edit(&self) {
        unsafe {
            let field = self.search_rect();
            let line = 20.0;
            let (x, y, w, h) = (field.x + 36.0, field.y + (field.h - line) / 2.0, field.w - 48.0, line);
            let _ = SetWindowPos(
                self.edit,
                None,
                px(x, self.dpi),
                px(y, self.dpi),
                px(w, self.dpi),
                px(h, self.dpi),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.edit, if self.tab == 0 { SW_SHOWNA } else { SW_HIDE });
        }
    }

    fn search_rect(&self) -> Rect {
        Rect::new(MARGIN, HEADER, self.size.0 - 2.0 * MARGIN, 38.0)
    }

    fn arm_confirm(&mut self) {
        self.confirm_clear = Some(Instant::now());
        unsafe {
            SetTimer(Some(self.hwnd), 3, 3000, None);
        }
    }

    fn set_tab(&mut self, tab: usize) {
        if tab != self.tab {
            self.tab = tab;
            self.confirm_clear = None;
            self.layout_edit();
            unsafe {
                let _ = SetFocus(Some(if tab == 0 { self.edit } else { self.hwnd }));
            }
        }
    }

    pub fn on_query_changed(&mut self) {
        unsafe {
            let len = GetWindowTextLengthW(self.edit) as usize;
            let mut buffer = vec![0u16; len + 1];
            let copied = GetWindowTextW(self.edit, &mut buffer) as usize;
            self.query = String::from_utf16_lossy(&buffer[..copied]).trim().to_lowercase();
        }
        self.selected = 0;
        self.scroll[0].target = 0.0;
        self.history_changed();
    }

    fn rebuild_view(&mut self, store: &History) {
        let kinds = FILTERS[self.filter].1;
        self.view.clear();
        for (index, entry) in store.items.iter().enumerate() {
            let kind_ok = kinds.is_none_or(|k| k.contains(&entry.kind));
            if kind_ok && (contains_ci(&entry.preview, &self.query) || contains_ci(&entry.full, &self.query)) {
                self.view.push(index);
            }
        }
        self.selected = self.selected.min(self.view.len().saturating_sub(1));
        self.view_dirty = false;
    }

    pub fn on_key(&mut self, vk: u16) -> Option<Action> {
        if vk == VK_ESCAPE.0 {
            unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            return None;
        }
        if self.tab != 0 {
            return None;
        }
        match vk {
            v if v == VK_UP.0 => self.select(-1),
            v if v == VK_DOWN.0 => self.select(1),
            v if v == VK_RETURN.0 => return self.view.get(self.selected).map(|&i| Action::Copy(i, true)),
            _ => {}
        }
        None
    }

    fn select(&mut self, delta: i32) {
        if self.view.is_empty() {
            return;
        }
        self.selected = (self.selected as i32 + delta).clamp(0, self.view.len() as i32 - 1) as usize;
        let top = self.selected as f32 * (ROW + ROW_GAP);
        let scroll = &mut self.scroll[0];
        if top < scroll.target {
            scroll.target = top;
        } else if top + ROW > scroll.target + self.list_height {
            scroll.target = top + ROW - self.list_height;
        }
        self.invalidate();
    }

    pub fn flash(&mut self, entry: &Entry) {
        self.flash = Some((entry.id.clone(), Instant::now()));
        unsafe {
            SetTimer(Some(self.hwnd), 2, 1100, None);
        }
        self.invalidate();
    }

    fn set_anim_timer(&mut self, on: bool) {
        if on != self.timer_on {
            unsafe {
                if on {
                    SetTimer(Some(self.hwnd), 1, 16, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), 1);
                }
            }
            self.timer_on = on;
        }
    }

    fn find_hot(&self) -> u32 {
        self.hits.iter().rev().find(|(r, _)| r.contains(self.mouse.0, self.mouse.1)).map_or(0, |h| h.1)
    }

    pub fn on_message(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        let point = || {
            let (x, y) = ((lp.0 & 0xFFFF) as i16 as i32, (lp.0 >> 16 & 0xFFFF) as i16 as i32);
            (dips(x, self.dpi), dips(y, self.dpi))
        };
        match msg {
            WM_SIZE => {
                let (w, h) = ((lp.0 & 0xFFFF) as u32, (lp.0 >> 16 & 0xFFFF) as u32);
                if w > 0 && h > 0 {
                    unsafe {
                        let _ = self.hrt.Resize(&D2D_SIZE_U { width: w, height: h });
                    }
                    self.size = (dips(w as i32, self.dpi), dips(h as i32, self.dpi));
                    self.layout_edit();
                    self.invalidate();
                }
            }
            WM_DPICHANGED => unsafe {
                let dpi = (wp.0 & 0xFFFF) as u32;
                self.dpi = dpi;
                self.hrt.SetDpi(dpi as f32, dpi as f32);
                let font = create_edit_font(dpi);
                SendMessageW(self.edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
                let _ = DeleteObject(self.edit_font.into());
                self.edit_font = font;
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            },
            WM_MOUSEMOVE => {
                let first_move = self.mouse.0 < 0.0;
                self.mouse = point();
                if first_move {
                    unsafe {
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: self.hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut track);
                    }
                }
                if self.active != 0 {
                    self.invalidate();
                } else {
                    let hot = self.find_hot();
                    if hot != self.hot {
                        self.hot = hot;
                        self.invalidate();
                    }
                }
            }
            WM_MOUSELEAVE => {
                self.mouse = (-1.0, -1.0);
                if self.hot != 0 && self.active == 0 {
                    self.hot = 0;
                    self.invalidate();
                }
            }
            WM_LBUTTONDOWN => {
                self.mouse = point();
                self.hot = self.find_hot();
                self.active = self.hot;
                unsafe {
                    SetCapture(self.hwnd);
                    if self.tab == 0 {
                        let _ = SetFocus(Some(self.edit));
                    }
                }
                self.invalidate();
            }
            WM_LBUTTONUP => {
                self.mouse = point();
                let over = self.find_hot();
                if self.active != 0 {
                    self.released = self.active;
                    if over == self.active {
                        self.clicked = self.active;
                    }
                }
                self.active = 0;
                self.hot = over;
                unsafe {
                    let _ = ReleaseCapture();
                }
                self.invalidate();
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as i16 as f32 / 120.0;
                let scroll = &mut self.scroll[self.tab];
                scroll.target = (scroll.target - delta * 72.0).clamp(0.0, scroll.max);
                self.invalidate();
            }
            WM_KEYDOWN => {
                if wp.0 as u16 == VK_ESCAPE.0 {
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                }
            }
            WM_SETCURSOR => {
                if (lp.0 & 0xFFFF) as u32 == HTCLIENT && self.hot != 0 {
                    unsafe {
                        if let Ok(hand) = LoadCursorW(None, IDC_HAND) {
                            SetCursor(Some(hand));
                        }
                    }
                    return Some(LRESULT(1));
                }
                return None;
            }
            WM_TIMER => match wp.0 {
                1 => self.invalidate(),
                2 => unsafe {
                    let _ = KillTimer(Some(self.hwnd), 2);
                    self.flash = None;
                    self.invalidate();
                },
                3 => unsafe {
                    let _ = KillTimer(Some(self.hwnd), 3);
                    self.confirm_clear = None;
                    self.invalidate();
                },
                _ => return None,
            },
            WM_COMMAND => {
                if (wp.0 >> 16) as u32 == EN_CHANGE {
                    self.on_query_changed();
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }

    fn hit(&mut self, rect: Rect, id: u32) -> Hit {
        let visible = rect.intersect(&self.clip);
        if visible.w <= 0.0 || visible.h <= 0.0 {
            return Hit::default();
        }
        self.hits.push((visible, id));
        Hit { hot: self.hot == id, active: self.active == id, clicked: self.clicked == id, released: self.released == id }
    }

    fn anim(&mut self, id: u32, target: f32, rate: f32) -> f32 {
        let dt = self.dt;
        let value = self.anims.entry(id).or_insert(target);
        let delta = target - *value;
        if delta.abs() < 0.002 {
            *value = target;
        } else {
            *value += delta * (1.0 - (-dt * rate).exp());
            self.animating = true;
        }
        *value
    }

    pub fn paint(&mut self, store: &History) -> Output {
        let now = Instant::now();
        self.dt = (now - self.last_paint).as_secs_f32().min(0.05);
        self.last_paint = now;
        self.animating = false;
        self.hits.clear();
        self.clip = Rect::new(0.0, 0.0, self.size.0, self.size.1);
        if self.view_dirty {
            self.rebuild_view(store);
        }

        unsafe {
            self.c.rt.BeginDraw();
            self.c.rt.Clear(Some(&color(BG, 1.0)));
        }
        self.header();
        if self.tab == 0 {
            self.history_tab(store);
        } else {
            self.settings_tab();
        }
        if unsafe { self.c.rt.EndDraw(None, None) }.is_err() {
            self.rebuild_target();
        }

        let interacted = self.clicked != 0 || self.released != 0;
        self.clicked = 0;
        self.released = 0;
        let hot = if self.active == 0 { self.find_hot() } else { self.hot };
        if interacted || hot != self.hot || self.animating {
            self.hot = hot;
            self.invalidate();
        }
        self.set_anim_timer(self.animating);
        std::mem::take(&mut self.output)
    }

    fn rebuild_target(&mut self) {
        let gfx = self.c.gfx.clone();
        let Ok(hrt) = create_target(&gfx, self.hwnd, self.dpi) else { return };
        let Ok(rt) = windows::core::Interface::cast(&hrt) else { return };
        let Ok(canvas) = Canvas::new(rt, gfx) else { return };
        self.hrt = hrt;
        self.c = canvas;
        self.thumbs.clear();
        self.invalidate();
    }

    fn header(&mut self) {
        let w = self.size.0;
        self.c.text("Copy HUD", Face::UiBold, 18.0, Rect::new(MARGIN, 8.0, 140.0, 42.0), TEXT, Align::Left);
        if config::get().paused {
            let chip = Rect::new(MARGIN + 92.0, 20.0, 56.0, 19.0);
            self.c.fill_rr(chip, 9.5, 0x1C1C1C, 1.0);
            self.c.text("PAUSED", Face::UiBold, 9.5, chip, TEXT2, Align::Center);
        }
        let (tw, th) = (86.0, 30.0);
        let group = Rect::new(w - MARGIN - tw * 2.0 - 4.0, 14.0, tw * 2.0 + 4.0, th + 4.0);
        self.c.fill_rr(group, 11.0, 0x111111, 1.0);
        let position = self.anim(id::ANIM_TAB, self.tab as f32, 24.0);
        let knob = Rect::new(group.x + 2.0 + position * tw, group.y + 2.0, tw, th);
        self.c.fill_rr(knob, 9.0, 0x262626, 1.0);
        for (index, label) in ["History", "Settings"].into_iter().enumerate() {
            let rect = Rect::new(group.x + 2.0 + index as f32 * tw, group.y + 2.0, tw, th);
            let hit = self.hit(rect, id::TAB + index as u32);
            if hit.clicked {
                self.set_tab(index);
            }
            let fg = if self.tab == index || hit.hot { TEXT } else { TEXT2 };
            self.c.text(label, Face::UiBold, 12.5, rect, fg, Align::Center);
        }
    }

    fn advance_scroll(&mut self, tab: usize, content: f32, view: f32) {
        let dt = self.dt;
        let scroll = &mut self.scroll[tab];
        scroll.max = (content - view).max(0.0);
        scroll.target = scroll.target.clamp(0.0, scroll.max);
        let delta = scroll.target - scroll.pos;
        if delta.abs() < 0.25 {
            scroll.pos = scroll.target;
        } else {
            scroll.pos += delta * (1.0 - (-dt * 20.0).exp());
            self.animating = true;
        }
    }

    fn scrollbar(&mut self, tab: usize, area: Rect, content: f32) {
        let scroll = self.scroll[tab];
        if scroll.max <= 0.0 {
            return;
        }
        let track = Rect::new(area.x + area.w - 8.0, area.y + 2.0, 8.0, area.h - 4.0);
        let thumb_h = (area.h / content * track.h).max(32.0);
        let hit = self.hit(track, id::SCROLLBAR);
        if hit.active {
            let travel = (track.h - thumb_h).max(1.0);
            let t = ((self.mouse.1 - track.y - thumb_h / 2.0) / travel).clamp(0.0, 1.0);
            self.scroll[tab].target = t * scroll.max;
            self.scroll[tab].pos = t * scroll.max;
        }
        let thumb_y = track.y + (track.h - thumb_h) * (scroll.pos / scroll.max);
        let width = if hit.hot || hit.active { 5.0 } else { 3.0 };
        let fill = if hit.hot || hit.active { 0x5A5A5A } else { 0x333333 };
        self.c.fill_rr(Rect::new(track.x + 8.0 - width - 1.0, thumb_y, width, thumb_h), width / 2.0, fill, 1.0);
    }

    fn history_tab(&mut self, store: &History) {
        let (w, h) = self.size;
        let field = self.search_rect();
        self.c.fill_rr(field, 11.0, FIELD, 1.0);
        self.c.icon(glyph::SEARCH, 13.0, Rect::new(field.x + 6.0, field.y, 30.0, field.h), TEXT3);

        let mut x = MARGIN;
        let chip_y = field.y + field.h + 10.0;
        for (index, (label, _)) in FILTERS.iter().enumerate() {
            let chip_w = (self.c.gfx.measure(label, Face::Ui, 12.0) + 20.0).ceil();
            let rect = Rect::new(x, chip_y, chip_w, 26.0);
            let hit = self.hit(rect, id::FILTER + index as u32);
            if hit.clicked && self.filter != index {
                self.filter = index;
                self.selected = 0;
                self.scroll[0].target = 0.0;
                self.rebuild_view(store);
            }
            let selected = self.filter == index;
            if selected {
                self.c.fill_rr(rect, 13.0, TEXT, 1.0);
            } else if hit.hot {
                self.c.fill_rr(rect, 13.0, 0x1A1A1A, 1.0);
            }
            let fg = if selected {
                0x000000
            } else if hit.hot {
                TEXT
            } else {
                TEXT2
            };
            self.c.text(label, Face::Ui, 12.0, rect, fg, Align::Center);
            x += chip_w + 4.0;
        }

        let list_top = chip_y + 26.0 + 10.0;
        let list = Rect::new(MARGIN - 4.0, list_top, w - 2.0 * MARGIN + 8.0, h - list_top - FOOTER);
        self.list_height = list.h;
        let content = self.view.len() as f32 * (ROW + ROW_GAP) - ROW_GAP;
        self.advance_scroll(0, content.max(0.0), list.h);
        let offset = self.scroll[0].pos;

        self.c.push_clip(list);
        self.clip = list;
        if self.view.is_empty() {
            self.empty_state(list, store.items.is_empty());
        }
        let first = (offset / (ROW + ROW_GAP)) as usize;
        let last = (((offset + list.h) / (ROW + ROW_GAP)).ceil() as usize + 1).min(self.view.len());
        for position in first..last {
            let index = self.view[position];
            let top = list.y + position as f32 * (ROW + ROW_GAP) - offset;
            self.history_row(&store.items[index], index, position, Rect::new(list.x, top, list.w - 6.0, ROW));
        }
        self.scrollbar(0, list, content);
        self.c.pop_clip();
        self.clip = Rect::new(0.0, 0.0, w, h);

        let footer_y = h - FOOTER;
        self.c.fill(Rect::new(MARGIN, footer_y, w - 2.0 * MARGIN, 1.0), LINE, 1.0);
        let shown = self.view.len();
        let count = if shown == store.items.len() {
            format!("{shown} {}", if shown == 1 { "item" } else { "items" })
        } else {
            format!("{shown} of {}", store.items.len())
        };
        self.c.text(&count, Face::Ui, 11.5, Rect::new(MARGIN, footer_y, 80.0, FOOTER), TEXT3, Align::Left);
        self.c.text(
            "\u{2191}\u{2193} select   \u{21B5} copy   Esc close",
            Face::Ui,
            11.5,
            Rect::new(MARGIN + 74.0, footer_y, w - 2.0 * MARGIN - 160.0, FOOTER),
            TEXT3,
            Align::Center,
        );

        if !store.items.is_empty() {
            let confirming = self.confirm_clear.is_some();
            let label = if confirming { "Really clear?" } else { "Clear all" };
            let width = 86.0;
            let rect = Rect::new(w - MARGIN - width, footer_y + 6.0, width, FOOTER - 12.0);
            let hit = self.hit(rect, id::CLEAR);
            if hit.clicked {
                if confirming {
                    self.confirm_clear = None;
                    self.output.actions.push(Action::Clear);
                } else {
                    self.arm_confirm();
                }
            }
            if hit.hot || confirming {
                self.c.fill_rr(rect, 8.0, if confirming { TEXT } else { 0x1A1A1A }, 1.0);
            }
            let fg = if confirming {
                0x000000
            } else if hit.hot {
                TEXT
            } else {
                TEXT2
            };
            self.c.text(label, Face::Ui, 11.5, rect, fg, Align::Center);
        }
    }

    fn empty_state(&mut self, list: Rect, nothing_yet: bool) {
        let (headline, detail) = if nothing_yet {
            ("Nothing copied yet", "Copy anything and it shows up here.")
        } else {
            ("No matches", "Try a different search or filter.")
        };
        let mid = list.y + list.h * 0.38;
        self.c.icon(glyph::CLIP, 26.0, Rect::new(list.x, mid - 44.0, list.w, 40.0), 0x4A4A4A);
        self.c.text(headline, Face::UiBold, 14.0, Rect::new(list.x, mid, list.w, 24.0), TEXT2, Align::Center);
        self.c.text(detail, Face::Ui, 12.0, Rect::new(list.x, mid + 24.0, list.w, 20.0), TEXT3, Align::Center);
    }

    fn thumb(&mut self, key: &Arc<str>) -> Option<ID2D1Bitmap> {
        if let Some(found) = self.thumbs.get(key) {
            return found.clone();
        }
        let bitmap = history::load_thumb(key).and_then(|t| self.c.create_bitmap(&t));
        self.thumbs.insert(key.clone(), bitmap.clone());
        bitmap
    }

    fn history_row(&mut self, entry: &Entry, index: usize, position: usize, rect: Rect) {
        let row_id = id::ROW + index as u32;
        let delete_id = id::DELETE + index as u32;
        let delete_rect = Rect::new(rect.x + rect.w - 40.0, rect.y + (ROW - 30.0) / 2.0, 30.0, 30.0);
        let row = self.hit(rect, row_id);
        let delete = self.hit(delete_rect, delete_id);
        let keyboard_selected = position == self.selected;

        if row.clicked {
            self.selected = position;
            self.output.actions.push(Action::Copy(index, false));
        }
        if delete.clicked {
            self.output.actions.push(Action::Delete(index));
        }

        let hovered = row.hot || delete.hot;
        if keyboard_selected {
            self.c.fill_rr(rect, 10.0, SELECTED, 1.0);
        } else if hovered {
            self.c.fill_rr(rect, 10.0, HOVER, 1.0);
        }

        let tile = Rect::new(rect.x + 10.0, rect.y + 10.0, 36.0, 36.0);
        self.row_tile(entry, tile);

        let flashing = self.flash.as_ref().is_some_and(|(id, _)| *id == entry.id);
        let text_x = tile.x + tile.w + 12.0;
        let text_w = rect.x + rect.w - text_x - if hovered || flashing { 48.0 } else { 12.0 };
        let face = if entry.kind == Kind::Code { Face::Mono } else { Face::Ui };
        let size = if entry.kind == Kind::Code { 12.5 } else { 13.5 };
        self.c.text(&entry.preview, face, size, Rect::new(text_x, rect.y + 9.0, text_w, 20.0), TEXT, Align::Left);

        let mut meta = format!("{}  \u{00B7}  {}", entry.kind.label(), relative_time(entry.timestamp));
        if matches!(entry.kind, Kind::Text | Kind::Code | Kind::Files) && !entry.badge.is_empty() {
            meta.push_str("  \u{00B7}  ");
            meta.push_str(&entry.badge);
        }
        self.c.text(&meta, Face::Ui, 11.5, Rect::new(text_x, rect.y + 29.0, text_w, 17.0), TEXT3, Align::Left);

        if flashing {
            self.c.icon(glyph::CHECK, 14.0, delete_rect, TEXT);
        } else if hovered {
            if delete.hot {
                self.c.fill_rr(delete_rect, 8.0, 0x242424, 1.0);
            }
            self.c.icon(glyph::CLOSE, 11.0, delete_rect, if delete.hot { TEXT } else { TEXT3 });
        }
    }

    fn row_tile(&mut self, entry: &Entry, tile: Rect) {
        if entry.kind == Kind::Color
            && let Some([r, g, b]) = crate::classify::parse_color(&entry.full)
        {
            let rgb = (r as u32) << 16 | (g as u32) << 8 | b as u32;
            self.c.fill_rr(tile, 9.0, rgb, 1.0);
            self.c.stroke_rr(tile.inset(0.5), 8.5, TEXT, 0.22, 1.0);
            return;
        }
        if let Some(key) = &entry.image
            && let Some(bitmap) = self.thumb(key)
        {
            self.c.bitmap_rr(&bitmap, tile, 9.0);
            self.c.stroke_rr(tile.inset(0.5), 8.5, TEXT, 0.14, 1.0);
            return;
        }
        self.c.fill_rr(tile, 9.0, 0x161616, 1.0);
        self.c.icon(row_glyph(entry.kind), 15.0, tile, 0xD8D8D8);
    }

    fn settings_tab(&mut self) {
        let (w, h) = self.size;
        let area = Rect::new(0.0, HEADER - 4.0, w, h - HEADER + 4.0);
        let offset = self.scroll[1].pos;
        self.c.push_clip(area);
        self.clip = area;

        let before = config::get();
        let mut c = before;
        let start = area.y + 6.0 - offset;
        let mut y = start;
        let width = w - 2.0 * MARGIN;
        let x = MARGIN;

        let test = Rect::new(x, y, width, 42.0);
        let hit = self.hit(test, id::TEST);
        if hit.clicked {
            overlay::send(Cmd::Test);
        }
        self.c.fill_rr(
            test,
            11.0,
            if hit.active {
                0xC8C8C8
            } else if hit.hot {
                0xE8E8E8
            } else {
                TEXT
            },
            1.0,
        );
        self.c.text("Preview the pills", Face::UiBold, 13.0, test, 0x000000, Align::Center);
        y += 42.0;

        y = self.section("Placement", y);
        self.c.text("Screen position", Face::Ui, 13.0, Rect::new(x, y, 150.0, 92.0), TEXT, Align::Left);
        self.anchor_picker(&mut c, Rect::new(x + width - 148.0, y + 4.0, 148.0, 84.0));
        y += 96.0;
        let mut preview_needed = false;
        y = self.switch(1, "Follow the active display", Some("Show pills on the monitor under your cursor"), &mut c.follow_cursor, y);
        y = self.int_slider(0, "Stack limit", "", &mut c.stack_limit, 1, 6, y, &mut preview_needed);
        let mut margin = c.screen_margin as usize;
        y = self.int_slider(1, "Edge margin", " px", &mut margin, 10, 300, y, &mut preview_needed);
        c.screen_margin = margin as i32;
        let mut preview_len = c.max_preview_length;
        y = self.int_slider(2, "Preview length", " chars", &mut preview_len, 12, 120, y, &mut preview_needed);
        c.max_preview_length = preview_len;

        y = self.section("Appearance", y);
        y = self.float_slider(3, "Pill size", 100.0, "%", &mut c.scale, 0.7, 1.7, y, &mut preview_needed);
        y = self.float_slider(4, "Text size", 100.0, "%", &mut c.font_scale, 0.7, 1.5, y, &mut preview_needed);
        y = self.float_slider(5, "Opacity", 100.0, "%", &mut c.opacity, 0.3, 1.0, y, &mut preview_needed);
        y = self.float_slider(6, "Display time", 1.0, " s", &mut c.popup_duration, 0.5, 10.0, y, &mut preview_needed);
        y = self.switch(2, "Type icon", None, &mut c.show_icon, y);
        y = self.switch(3, "Size badge", None, &mut c.show_char_count, y);

        y = self.section("Sound", y);
        y = self.sound_picker(&mut c, y);
        let mut volume = c.sound_volume as usize;
        let mut volume_released = false;
        y = self.int_slider(7, "Volume", "%", &mut volume, 0, 100, y, &mut volume_released);
        c.sound_volume = volume as u32;

        y = self.section("Detection", y);
        y = self.switch(4, "Color swatches", Some("Show hex and rgb() colors as a live swatch"), &mut c.detect_colors, y);
        y = self.switch(5, "Image thumbnails", Some("Preview screenshots and copied images"), &mut c.detect_images, y);

        y = self.section("Behavior", y);
        y = self.switch(6, "Click-through", Some("Pills ignore the mouse entirely"), &mut c.click_through, y);
        y = self.switch(7, "Keep history on disk", Some("Off keeps it in memory until you quit"), &mut c.keep_history, y);
        y = self.switch(8, "Pause monitoring", None, &mut c.paused, y);
        let mut autostart = self.autostart;
        y = self.switch(9, "Start with Windows", None, &mut autostart, y);
        if autostart != self.autostart {
            set_autostart(autostart);
            self.autostart = autostart;
        }
        y = self.switch(10, "Tray icon", Some("When hidden, open with Alt+Shift+C or by launching the app"), &mut c.show_tray, y);

        y += 12.0;
        let hint = if self.hotkey_ok { "Alt+Shift+C opens this panel from anywhere." } else { "Alt+Shift+C is in use by another app." };
        self.c.text(hint, Face::Ui, 11.5, Rect::new(x, y, width - 164.0, 28.0), TEXT3, Align::Left);
        let quit = Rect::new(x + width - 92.0 - 6.0 - 56.0, y, 56.0, 28.0);
        let hit = self.hit(quit, id::QUIT);
        if hit.clicked {
            self.output.actions.push(Action::Quit);
        }
        if hit.hot {
            self.c.fill_rr(quit, 8.0, 0x1A1A1A, 1.0);
        }
        self.c.text("Quit", Face::Ui, 11.5, quit, if hit.hot { TEXT } else { TEXT2 }, Align::Center);
        let reset = Rect::new(x + width - 92.0, y, 92.0, 28.0);
        let hit = self.hit(reset, id::RESET);
        let confirming = self.confirm_clear.is_some();
        if hit.clicked {
            if confirming {
                self.confirm_clear = None;
                c = config::DEFAULT;
            } else {
                self.arm_confirm();
            }
        }
        if hit.hot || confirming {
            self.c.fill_rr(reset, 8.0, if confirming { TEXT } else { 0x1A1A1A }, 1.0);
        }
        let fg = if confirming {
            0x000000
        } else if hit.hot {
            TEXT
        } else {
            TEXT2
        };
        self.c.text(if confirming { "Really reset?" } else { "Reset all" }, Face::Ui, 11.5, reset, fg, Align::Center);
        y += 28.0 + 6.0;

        let credit = Rect::new(x, y, width, 30.0);
        let hit = self.hit(credit, id::CREDIT);
        if hit.clicked {
            unsafe {
                ShellExecuteW(None, w!("open"), w!("https://github.com/visiuun/copyconfirm"), None, None, SW_SHOWNORMAL);
            }
        }
        let label = format!("Copy HUD {}  \u{00B7}  Created by visiuun", env!("CARGO_PKG_VERSION"));
        self.c.text(&label, Face::Ui, 11.5, credit, if hit.hot { TEXT } else { TEXT3 }, Align::Center);
        y += 30.0 + 12.0;

        let content = y - start;
        self.advance_scroll(1, content, area.h);
        self.scrollbar(1, area, content);
        self.c.pop_clip();
        self.clip = Rect::new(0.0, 0.0, w, h);

        if c != before {
            let history_policy = c.keep_history != before.keep_history;
            config::set(c);
            overlay::send(Cmd::ConfigChanged);
            self.output.config_changed = true;
            self.output.history_policy_changed |= history_policy;
        }
        if preview_needed {
            overlay::send(Cmd::Show(PillData::notice("This is how your pills look", "PREVIEW")));
        }
        if volume_released {
            overlay::send(Cmd::PlaySound);
        }
    }

    fn section(&mut self, title: &str, y: f32) -> f32 {
        let y = y + 20.0;
        self.c.text(&title.to_uppercase(), Face::UiBold, 10.5, Rect::new(MARGIN, y, 200.0, 18.0), TEXT3, Align::Left);
        y + 24.0
    }

    fn anchor_picker(&mut self, c: &mut Config, screen: Rect) {
        self.c.fill_rr(screen, 9.0, 0x0B0B0B, 1.0);
        self.c.stroke_rr(screen.inset(0.5), 8.5, 0x2A2A2A, 1.0, 1.0);
        let (cell_w, cell_h) = (screen.w / 3.0, screen.h / 2.0);
        for (index, anchor) in Anchor::ALL.into_iter().enumerate() {
            let (col, row) = (index as f32 % 3.0, (index / 3) as f32);
            let cell = Rect::new(screen.x + col * cell_w, screen.y + row * cell_h, cell_w, cell_h);
            let hit = self.hit(cell, id::ANCHOR + index as u32);
            if hit.clicked {
                c.position = anchor;
                overlay::send(Cmd::Show(PillData::notice(anchor.name(), "PREVIEW")));
            }
            let selected = c.position == anchor;
            let (pill_w, pill_h) = (28.0, 8.0);
            let px_x = cell.x + (cell.w - pill_w) / 2.0;
            let px_y = if row == 0.0 { cell.y + 10.0 } else { cell.y + cell.h - 10.0 - pill_h };
            let fill = if selected {
                TEXT
            } else if hit.hot {
                0x6A6A6A
            } else {
                0x2E2E2E
            };
            self.c.fill_rr(Rect::new(px_x, px_y, pill_w, pill_h), pill_h / 2.0, fill, 1.0);
        }
    }

    fn sound_picker(&mut self, c: &mut Config, mut y: f32) -> f32 {
        let right = self.size.0 - MARGIN;
        let mut x = MARGIN;
        for (index, name) in sound::NAMES.iter().enumerate() {
            let chip_w = (self.c.gfx.measure(name, Face::Ui, 12.0) + 24.0).ceil();
            if x + chip_w > right {
                x = MARGIN;
                y += 34.0;
            }
            let rect = Rect::new(x, y, chip_w, 28.0);
            let hit = self.hit(rect, id::SOUND + index as u32);
            if hit.clicked {
                c.sound = index;
                config::set(*c);
                overlay::send(Cmd::ConfigChanged);
                overlay::send(Cmd::PlaySound);
            }
            let selected = c.sound == index;
            if selected {
                self.c.fill_rr(rect, 14.0, TEXT, 1.0);
            } else {
                self.c.fill_rr(rect, 14.0, if hit.hot { 0x202020 } else { 0x121212 }, 1.0);
            }
            self.c.text(name, Face::Ui, 12.0, rect, if selected { 0x000000 } else { 0xD0D0D0 }, Align::Center);
            x += chip_w + 6.0;
        }
        y + 28.0 + 12.0
    }

    fn slider_row(&mut self, index: u32, label: &str, value_text: &str, mut t: f32, y: f32) -> (f32, bool, bool) {
        let width = self.size.0 - 2.0 * MARGIN;
        self.c.text(label, Face::Ui, 13.0, Rect::new(MARGIN, y, width - 90.0, 24.0), TEXT, Align::Left);
        self.c.text(value_text, Face::Ui, 12.5, Rect::new(MARGIN + width - 90.0, y, 90.0, 24.0), TEXT2, Align::Right);
        let track_y = y + 36.0;
        let grab = Rect::new(MARGIN - 6.0, track_y - 11.0, width + 12.0, 22.0);
        let hit = self.hit(grab, id::SLIDER + index);
        let mut changed = false;
        if hit.active {
            let next = ((self.mouse.0 - MARGIN) / width).clamp(0.0, 1.0);
            changed = next != t;
            t = next;
        }
        self.c.fill_rr(Rect::new(MARGIN, track_y - 2.0, width, 4.0), 2.0, TRACK, 1.0);
        self.c.fill_rr(Rect::new(MARGIN, track_y - 2.0, width * t, 4.0), 2.0, TEXT, 1.0);
        let radius = self.anim(
            id::SLIDER + 500 + index,
            if hit.active {
                9.0
            } else if hit.hot {
                8.0
            } else {
                7.0
            },
            26.0,
        );
        self.c.fill_circle(MARGIN + width * t, track_y, radius, TEXT, 1.0);
        (t, changed, hit.released)
    }

    #[allow(clippy::too_many_arguments)]
    fn int_slider(
        &mut self,
        index: u32,
        label: &str,
        unit: &str,
        value: &mut usize,
        min: usize,
        max: usize,
        y: f32,
        released: &mut bool,
    ) -> f32 {
        let t = (*value - min) as f32 / (max - min) as f32;
        let (next, changed, done) = self.slider_row(index, label, &format!("{value}{unit}"), t, y);
        if changed {
            *value = min + (next * (max - min) as f32).round() as usize;
        }
        *released |= done;
        y + 56.0
    }

    #[allow(clippy::too_many_arguments)]
    fn float_slider(
        &mut self,
        index: u32,
        label: &str,
        factor: f32,
        unit: &str,
        value: &mut f32,
        min: f32,
        max: f32,
        y: f32,
        released: &mut bool,
    ) -> f32 {
        let t = (*value - min) / (max - min);
        let shown = if factor > 1.0 { format!("{:.0}{unit}", *value * factor) } else { format!("{:.1}{unit}", *value) };
        let (next, changed, done) = self.slider_row(index, label, &shown, t, y);
        if changed {
            *value = min + next * (max - min);
        }
        *released |= done;
        y + 56.0
    }

    fn switch(&mut self, index: u32, label: &str, detail: Option<&str>, value: &mut bool, y: f32) -> f32 {
        let width = self.size.0 - 2.0 * MARGIN;
        let height = if detail.is_some() { 52.0 } else { 40.0 };
        let hit = self.hit(Rect::new(MARGIN - 6.0, y, width + 12.0, height), id::SWITCH + index);
        if hit.clicked {
            *value = !*value;
        }
        let text_w = width - 60.0;
        match detail {
            Some(detail) => {
                self.c.text(label, Face::Ui, 13.0, Rect::new(MARGIN, y + 7.0, text_w, 20.0), TEXT, Align::Left);
                self.c.text(detail, Face::Ui, 11.5, Rect::new(MARGIN, y + 27.0, text_w, 17.0), TEXT3, Align::Left);
            }
            None => self.c.text(label, Face::Ui, 13.0, Rect::new(MARGIN, y, text_w, height), TEXT, Align::Left),
        }
        let (track_w, track_h) = (40.0, 22.0);
        let track = Rect::new(MARGIN + width - track_w, y + (height - track_h) / 2.0, track_w, track_h);
        let t = self.anim(id::SWITCH + 500 + index, *value as u8 as f32, 22.0);
        let lift = |from: f32, to: f32| from + (to - from) * t;
        let shade = lift(0x26 as f32, 0xFF as f32) as u32;
        self.c.fill_rr(track, track_h / 2.0, shade << 16 | shade << 8 | shade, 1.0);
        let knob_shade = lift(0xC8 as f32, 0x00 as f32) as u32;
        let cx = track.x + 11.0 + t * (track_w - track_h);
        self.c.fill_circle(cx, track.y + track_h / 2.0, 8.0, knob_shade << 16 | knob_shade << 8 | knob_shade, 1.0);
        y + height
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.edit_font.into());
        }
    }
}

pub fn stateless_message(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    unsafe {
        match msg {
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_CTLCOLOREDIT => {
                let dc = HDC(wp.0 as *mut _);
                SetTextColor(dc, COLORREF(0x00FFFFFF));
                SetBkColor(dc, COLORREF(FIELD));
                Some(LRESULT(EDIT_BRUSH.load(Ordering::Relaxed)))
            }
            WM_GETMINMAXINFO => {
                let dpi = GetDpiForWindow(hwnd).max(96);
                let info = &mut *(lp.0 as *mut MINMAXINFO);
                info.ptMinTrackSize = POINT { x: px(MIN_SIZE.0 as f32, dpi), y: px(MIN_SIZE.1 as f32, dpi) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}
