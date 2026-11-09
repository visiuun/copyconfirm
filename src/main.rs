#![cfg_attr(not(test), windows_subsystem = "windows")]

mod classify;
mod clipboard;
mod config;
mod gfx;
mod history;
mod imaging;
mod install;
mod json;
mod overlay;
mod panel;
mod sound;
mod win;

use gfx::Gfx;
use history::{Entry, History};
use overlay::{Cmd, PillData};
use panel::{Action, Output, Panel};
use std::cell::RefCell;
use std::sync::mpsc::{Receiver, channel};
use win::{NIN_KEYSELECT, NIN_SELECT, WM_APP_CLIP, WM_APP_KEY, WM_APP_OPEN, WM_APP_QUIT, WM_APP_TRAY};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, SetProcessWorkingSetSize};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCSTR, PCWSTR, w};

const HOST_CLASS: PCWSTR = w!("CopyHUD.Host");
const HOTKEY_ID: i32 = 1;
const TIMER_SAVE_HISTORY: usize = 1;
const TIMER_SAVE_CONFIG: usize = 2;
const MENU_OPEN: usize = 1;
const MENU_PAUSE: usize = 2;
const MENU_QUIT: usize = 3;

struct App {
    store: History,
    host: HWND,
    panel: Option<Box<Panel>>,
    entries: Receiver<Entry>,
    icon: HICON,
    taskbar_created: u32,
    tray_shown: bool,
    hotkey_ok: bool,
    history_dirty: bool,
    config_dirty: bool,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

// Re-entrant window messages see None and fall through to DefWindowProc.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok()?.as_mut().map(f))
}

fn tray_data(host: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: host, uID: 1, ..Default::default() }
}

impl App {
    fn sync_tray(&mut self) {
        let wanted = config::get().show_tray;
        if wanted == self.tray_shown {
            return;
        }
        if wanted {
            self.add_tray();
        } else {
            self.remove_tray();
        }
        self.tray_shown = wanted;
    }

    fn remove_tray(&self) {
        let data = tray_data(self.host);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
    }

    fn quit(&mut self) {
        self.flush();
        if self.tray_shown {
            self.remove_tray();
        }
        unsafe { PostQuitMessage(0) };
    }

    fn add_tray(&self) {
        let mut data = tray_data(self.host);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = WM_APP_TRAY;
        data.hIcon = self.icon;
        for (slot, unit) in data.szTip.iter_mut().zip("Copy HUD".encode_utf16()) {
            *slot = unit;
        }
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &data);
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }

    fn schedule_history_save(&mut self) {
        self.history_dirty = true;
        unsafe {
            SetTimer(Some(self.host), TIMER_SAVE_HISTORY, 900, None);
        }
    }

    fn schedule_config_save(&mut self) {
        self.config_dirty = true;
        unsafe {
            SetTimer(Some(self.host), TIMER_SAVE_CONFIG, 500, None);
        }
    }

    fn open_panel(&mut self, activate: bool, tab: usize) {
        if let Some(panel) = &self.panel {
            if activate {
                panel.focus();
            }
            return;
        }
        let Ok(gfx) = Gfx::new() else { return };
        if let Ok(mut panel) = Panel::open(gfx, tab, self.hotkey_ok) {
            let output = panel.present(&self.store, activate);
            self.panel = Some(panel);
            self.apply(output);
        }
    }

    fn close_panel(&mut self) {
        if self.panel.take().is_some() {
            unsafe {
                let _ = SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
            }
        }
    }

    fn toggle_panel(&mut self) {
        match &self.panel {
            Some(panel) if panel.is_foreground() => self.close_panel(),
            _ => self.open_panel(true, 0),
        }
    }

    fn drain_entries(&mut self) {
        let mut added = false;
        while let Ok(entry) = self.entries.try_recv() {
            self.store.add(entry);
            added = true;
        }
        if added {
            self.schedule_history_save();
            if let Some(panel) = &mut self.panel {
                panel.history_changed();
            }
        }
    }

    fn apply(&mut self, output: Output) {
        for action in output.actions {
            match action {
                Action::Copy(index, close) => {
                    let Some(entry) = self.store.items.get(index).cloned() else { continue };
                    if clipboard::restore(self.host, &entry) {
                        overlay::send(Cmd::Show(PillData::from_entry(&entry, history_thumb(&entry))));
                        if close {
                            self.close_panel();
                        } else if let Some(panel) = &mut self.panel {
                            panel.flash(&entry);
                        }
                    } else {
                        overlay::send(Cmd::Show(PillData::notice("That item is no longer available", "NOT COPIED")));
                    }
                }
                Action::Delete(index) => {
                    self.store.remove(index);
                    self.after_history_edit();
                }
                Action::Clear => {
                    self.store.clear();
                    self.after_history_edit();
                }
                Action::Quit => {
                    self.close_panel();
                    self.quit();
                    return;
                }
            }
        }
        if output.config_changed {
            self.schedule_config_save();
            self.sync_tray();
        }
        if output.history_policy_changed {
            if !config::get().keep_history {
                history::purge_images();
            }
            self.schedule_history_save();
        }
    }

    fn after_history_edit(&mut self) {
        self.schedule_history_save();
        if let Some(panel) = &mut self.panel {
            panel.history_changed();
        }
    }

    fn flush(&mut self) {
        if self.config_dirty {
            config::save();
            self.config_dirty = false;
        }
        self.store.save_now();
        self.history_dirty = false;
    }

    fn set_paused(&mut self, paused: bool) {
        let mut c = config::get();
        c.paused = paused;
        config::set(c);
        overlay::send(Cmd::ConfigChanged);
        self.schedule_config_save();
        if let Some(panel) = &self.panel {
            panel.invalidate();
        }
    }
}

fn history_thumb(entry: &Entry) -> Option<std::sync::Arc<imaging::Thumb>> {
    entry.image.as_deref().and_then(history::load_thumb).map(std::sync::Arc::new)
}

fn show_tray_menu(host: HWND, paused: bool) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return 0 };
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open Copy HUD\tAlt+Shift+C"));
        let pause_flags = if paused { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, pause_flags, MENU_PAUSE, w!("Pause monitoring"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let _ = SetMenuDefaultItem(menu, MENU_OPEN as u32, 0);
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(host);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, point.x, point.y, None, host, None);
        let _ = PostMessageW(Some(host), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        picked.0 as usize
    }
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_APP_QUIT => {
                with_app(App::quit);
                return LRESULT(0);
            }
            WM_HOTKEY => {
                with_app(App::toggle_panel);
                return LRESULT(0);
            }
            WM_APP_CLIP => {
                with_app(App::drain_entries);
                return LRESULT(0);
            }
            WM_APP_OPEN => {
                with_app(|a| a.open_panel(true, 0));
                return LRESULT(0);
            }
            WM_APP_TRAY => {
                match (lp.0 & 0xFFFF) as u32 {
                    NIN_SELECT | NIN_KEYSELECT => {
                        with_app(App::toggle_panel);
                    }
                    WM_CONTEXTMENU => {
                        // The menu runs a modal loop, so no app borrow may be held across it.
                        let picked = show_tray_menu(hwnd, config::get().paused);
                        with_app(|a| {
                            match picked {
                                MENU_OPEN => a.open_panel(true, 0),
                                MENU_PAUSE => a.set_paused(!config::get().paused),
                                MENU_QUIT => a.quit(),
                                _ => {}
                            }
                            a.drain_entries();
                        });
                    }
                    _ => {}
                }
                return LRESULT(0);
            }
            WM_TIMER => {
                with_app(|a| {
                    let _ = KillTimer(Some(a.host), wp.0);
                    match wp.0 {
                        TIMER_SAVE_HISTORY if a.history_dirty => {
                            a.store.save_async();
                            a.history_dirty = false;
                        }
                        TIMER_SAVE_CONFIG if a.config_dirty => {
                            config::save();
                            a.config_dirty = false;
                        }
                        _ => {}
                    }
                });
                return LRESULT(0);
            }
            WM_ENDSESSION if wp.0 != 0 => {
                with_app(App::flush);
                return LRESULT(0);
            }
            _ => {}
        }
        if with_app(|a| msg == a.taskbar_created && msg != 0).unwrap_or(false) {
            with_app(|a| {
                // Explorer restarted and forgot every icon.
                a.tray_shown = false;
                a.sync_tray();
            });
            return LRESULT(0);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if let Some(result) = panel::stateless_message(hwnd, msg, wp, lp) {
            return result;
        }
        let handled = with_app(|a| -> Option<LRESULT> {
            let panel = a.panel.as_mut().filter(|p| p.hwnd == hwnd)?;
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut ps);
                    let output = panel.paint(&a.store);
                    let _ = EndPaint(hwnd, &ps);
                    a.apply(output);
                    Some(LRESULT(0))
                }
                WM_CLOSE => {
                    a.close_panel();
                    Some(LRESULT(0))
                }
                WM_APP_KEY => {
                    if let Some(action) = panel.on_key(wp.0 as u16) {
                        a.apply(Output { actions: vec![action], ..Default::default() });
                    }
                    Some(LRESULT(0))
                }
                _ => panel.on_message(msg, wp, lp),
            }
        })
        .flatten();
        handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wp, lp))
    }
}

fn enable_dark_menus() {
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        // Undocumented but stable since 1903: SetPreferredAppMode is ordinal 135.
        if let Some(set_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
            let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
            set_mode(2);
        }
    }
}

fn register_class(name: PCWSTR, proc: WNDPROC, icon: HICON) {
    unsafe {
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: win::instance(),
            lpszClassName: name,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&wc);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let multi = std::env::var_os("COPYHUD_MULTI").is_some();

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        if install::hand_off(HOST_CLASS, &args) {
            return;
        }

        let _mutex = CreateMutexW(None, true, w!("Local\\CopyHUD.Singleton"));
        if !multi && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(HOST_CLASS, PCWSTR::null()) {
                let _ = PostMessageW(Some(existing), WM_APP_OPEN, WPARAM(0), LPARAM(0));
            }
            return;
        }

        config::load();
        enable_dark_menus();

        let icon = LoadIconW(Some(win::instance()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default();
        register_class(HOST_CLASS, Some(host_proc), icon);
        register_class(panel::CLASS, Some(panel_proc), icon);

        let Ok(host) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            HOST_CLASS,
            w!("Copy HUD Host"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(win::instance()),
            None,
        ) else {
            return;
        };

        let (entry_tx, entry_rx) = channel();
        overlay::spawn(host.0 as isize);
        clipboard::spawn(host.0 as isize, entry_tx);

        let hotkey_ok = RegisterHotKey(Some(host), HOTKEY_ID, MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, 'C' as u32).is_ok();

        let mut app = App {
            store: History::load(),
            host,
            panel: None,
            entries: entry_rx,
            icon,
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            tray_shown: false,
            hotkey_ok,
            history_dirty: false,
            config_dirty: false,
        };
        app.sync_tray();
        APP.with(|a| *a.borrow_mut() = Some(app));

        let ready = if hotkey_ok { "Copy HUD ready" } else { "Copy HUD ready (hotkey in use)" };
        overlay::send(Cmd::Show(PillData::notice(ready, "ALT+SHIFT+C")));

        // Opens without taking focus.
        if has("--open") || has("--open-settings") {
            with_app(|a| a.open_panel(false, has("--open-settings") as usize));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
