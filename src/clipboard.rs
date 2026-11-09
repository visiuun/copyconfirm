use crate::classify::{self, Kind};
use crate::config;
use crate::history::{self, Entry};
use crate::imaging::{self, Wic};
use crate::overlay::{self, Cmd, PillData};
use crate::win::{self, WM_APP_CLIP};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

const TEXT_STORE_LIMIT_BYTES: usize = 100_000;
const PREVIEW_CHARS: usize = 2048;
const STORED_WHEN_TRUNCATED: usize = 1500;
const MAX_PNG_PIXELS: u64 = 64_000_000;
const MAX_IMAGE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "jpg", "jpeg", "webp", "bmp", "gif", "ico", "tiff"];

/// Sequence number of our own last write, so restoring an item doesn't re-capture it.
static OWN_WRITE_SEQ: AtomicU32 = AtomicU32::new(0);

struct Formats {
    png: u32,
    exclude: u32,
    viewer_ignore: u32,
    can_include: u32,
}

fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| unsafe {
        Formats {
            png: RegisterClipboardFormatW(w!("PNG")),
            exclude: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
            viewer_ignore: RegisterClipboardFormatW(w!("Clipboard Viewer Ignore")),
            can_include: RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
        }
    })
}

struct Open;

impl Open {
    fn acquire(owner: Option<HWND>) -> Option<Open> {
        for _ in 0..10 {
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Some(Open);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        None
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

unsafe fn with_global<R>(handle: HANDLE, f: impl FnOnce(*const u8, usize) -> R) -> Option<R> {
    unsafe {
        let global = HGLOBAL(handle.0);
        let ptr = GlobalLock(global) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let result = f(ptr, GlobalSize(global));
        let _ = GlobalUnlock(global);
        Some(result)
    }
}

fn is_available(format: u32) -> bool {
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

// Formats that password managers set to keep a copy out of clipboard tools.
fn is_private() -> bool {
    let f = formats();
    if is_available(f.exclude) || is_available(f.viewer_ignore) {
        return true;
    }
    if is_available(f.can_include) {
        unsafe {
            if let Ok(handle) = GetClipboardData(f.can_include) {
                let value = with_global(handle, |p, n| if n >= 4 { (p as *const u32).read_unaligned() } else { 1 });
                return value == Some(0);
            }
        }
    }
    false
}

enum Raw {
    Text { head: String, stored: String, total: usize },
    Files(Vec<String>),
    Png(Vec<u8>),
    Dib(Vec<u8>),
    Private,
}

fn read_text(handle: HANDLE) -> Option<Raw> {
    unsafe {
        with_global(handle, |p, size| {
            if size < 2 {
                return None;
            }
            let units = std::slice::from_raw_parts(p as *const u16, size / 2);
            let stops_at = |slice: &[u16]| slice.iter().position(|&u| u == 0).unwrap_or(slice.len());
            if size < TEXT_STORE_LIMIT_BYTES {
                let text = &units[..stops_at(units)];
                let stored = String::from_utf16_lossy(text);
                let total = stored.chars().count();
                let end = stored.char_indices().nth(PREVIEW_CHARS).map_or(stored.len(), |(i, _)| i);
                return Some(Raw::Text { head: stored[..end].to_string(), stored, total });
            }
            let window = &units[..PREVIEW_CHARS.min(units.len())];
            let head = String::from_utf16_lossy(&window[..stops_at(window)]);
            let total = size / 2;
            let cut = head.char_indices().nth(STORED_WHEN_TRUNCATED).map_or(head.len(), |(i, _)| i);
            let stored = format!("{}\n... [Truncated: {} chars total]", &head[..cut], classify::grouped(total));
            Some(Raw::Text { head, stored, total })
        })?
    }
}

fn read_files(handle: HANDLE) -> Option<Raw> {
    unsafe {
        with_global(handle, |p, _| {
            let drop = HDROP(p as *mut _);
            let count = DragQueryFileW(drop, u32::MAX, None).min(4096);
            let mut paths = Vec::with_capacity(count as usize);
            for i in 0..count {
                let len = DragQueryFileW(drop, i, None) as usize;
                if len == 0 {
                    continue;
                }
                let mut buffer = vec![0u16; len + 1];
                let written = DragQueryFileW(drop, i, Some(&mut buffer)) as usize;
                paths.push(String::from_utf16_lossy(&buffer[..written]));
            }
            (!paths.is_empty()).then_some(Raw::Files(paths))
        })?
    }
}

fn read_bytes(handle: HANDLE) -> Option<Vec<u8>> {
    unsafe { with_global(handle, |p, n| std::slice::from_raw_parts(p, n).to_vec()).filter(|v| !v.is_empty()) }
}

fn read_raw() -> Option<Raw> {
    let _open = Open::acquire(None)?;
    if is_private() {
        return Some(Raw::Private);
    }
    unsafe {
        if is_available(CF_UNICODETEXT.0 as u32)
            && let Some(raw) = GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(read_text)
        {
            return Some(raw);
        }
        if is_available(CF_HDROP.0 as u32)
            && let Some(raw) = GetClipboardData(CF_HDROP.0 as u32).ok().and_then(read_files)
        {
            return Some(raw);
        }
        let png = formats().png;
        if is_available(png)
            && let Some(bytes) = GetClipboardData(png).ok().and_then(read_bytes)
        {
            return Some(Raw::Png(bytes));
        }
        if is_available(CF_DIB.0 as u32)
            && let Some(bytes) = GetClipboardData(CF_DIB.0 as u32).ok().and_then(read_bytes)
        {
            return Some(Raw::Dib(bytes));
        }
    }
    None
}

fn base_entry(kind: Kind, preview: String, badge: String, full: String) -> Entry {
    Entry {
        kind,
        preview: preview.into(),
        badge: badge.into(),
        full: full.into(),
        timestamp: history::now(),
        id: history::new_id(),
        image: None,
    }
}

struct Captured {
    entry: Option<Entry>,
    pill: PillData,
    fingerprint: String,
    deferred_png: Option<(Vec<u8>, String)>,
}

fn capture_bitmap(wic: &Wic, encoded: Vec<u8>) -> Option<Captured> {
    let cfg = config::get();
    let frame = wic.decode_memory(&encoded).ok()?;
    let (w, h) = imaging::frame_size(&frame);
    let thumb = if cfg.detect_images { wic.thumbnail(&frame).ok() } else { None };
    let key = format!("{:016x}", imaging::hash(&encoded));
    let mut entry = base_entry(Kind::Image, format!("Image ({w}\u{00D7}{h})"), "IMAGE".into(), format!("[Image {w}x{h}]"));
    let thumb = thumb.map(Arc::new);
    if let (Some(t), true) = (&thumb, cfg.keep_history) {
        history::save_thumb(&key, t);
    }
    entry.image = Some(key.as_str().into());
    let store = cfg.detect_images && cfg.keep_history && w as u64 * h as u64 <= MAX_PNG_PIXELS;
    Some(Captured {
        pill: PillData::from_entry(&entry, thumb),
        fingerprint: format!("image_{key}"),
        entry: Some(entry),
        deferred_png: store.then_some((encoded, key)),
    })
}

fn capture_files(wic: &Wic, paths: Vec<String>) -> Captured {
    let cfg = config::get();
    let first = paths[0].as_str();
    let name = Path::new(first).file_name().map_or_else(|| first.to_string(), |n| n.to_string_lossy().into_owned());
    let ext = Path::new(first).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let fingerprint = format!("files_{}", first);
    let joined = paths.join("\n");

    if cfg.detect_images && paths.len() == 1 && IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        let small_enough = std::fs::metadata(first).is_ok_and(|m| m.len() <= MAX_IMAGE_FILE_BYTES);
        if let Some(thumb) = small_enough.then(|| wic.decode_file(first).and_then(|f| wic.thumbnail(&f)).ok()).flatten() {
            let key = format!("{:016x}", imaging::hash(first.as_bytes()));
            if cfg.keep_history {
                history::save_thumb(&key, &thumb);
            }
            let mut entry = base_entry(Kind::Image, name, "PHOTO".into(), joined);
            entry.image = Some(key.into());
            return Captured {
                pill: PillData::from_entry(&entry, Some(Arc::new(thumb))),
                entry: Some(entry),
                fingerprint,
                deferred_png: None,
            };
        }
    }
    let count = paths.len();
    let preview = if count == 1 { name } else { format!("{name} (+{} more)", count - 1) };
    let badge = if count == 1 { "FILE".to_string() } else { format!("{count} FILES") };
    let entry = base_entry(Kind::Files, preview, badge, joined);
    Captured { pill: PillData::from_entry(&entry, None), entry: Some(entry), fingerprint, deferred_png: None }
}

fn capture(wic: &Wic) -> Option<Captured> {
    match read_raw()? {
        Raw::Private => Some(Captured { entry: None, pill: PillData::private(), fingerprint: "private".into(), deferred_png: None }),
        Raw::Text { head, stored, total } => {
            if head.trim().is_empty() {
                return None;
            }
            let c = classify::classify(&head, total);
            let fingerprint: String = format!("{}_{}", c.kind.key(), stored.chars().take(200).collect::<String>());
            let full = if c.kind == Kind::Color { head.trim().to_string() } else { stored };
            let entry = base_entry(c.kind, c.preview, c.badge, full);
            Some(Captured { pill: PillData::from_entry(&entry, None), entry: Some(entry), fingerprint, deferred_png: None })
        }
        Raw::Files(paths) => Some(capture_files(wic, paths)),
        Raw::Png(bytes) => capture_bitmap(wic, bytes),
        Raw::Dib(dib) => capture_bitmap(wic, imaging::bmp_from_dib(&dib)?),
    }
}

unsafe extern "system" fn listener_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub fn spawn(host: isize, entries: Sender<Entry>) {
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(wic) = Wic::new() else { return };
        let Ok(hwnd) = win::message_window(w!("CopyHUD.Listener"), Some(listener_proc)) else { return };
        if AddClipboardFormatListener(hwnd).is_err() {
            return;
        }
        let mut last: Option<(String, Instant)> = None;
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message != WM_CLIPBOARDUPDATE {
                let _ = DispatchMessageW(&msg);
                continue;
            }
            // Some apps write their formats in several bursts.
            std::thread::sleep(Duration::from_millis(25));
            while PeekMessageW(&mut msg, Some(hwnd), WM_CLIPBOARDUPDATE, WM_CLIPBOARDUPDATE, PM_REMOVE).as_bool() {}

            let seq = GetClipboardSequenceNumber();
            if config::get().paused || (seq != 0 && seq == OWN_WRITE_SEQ.load(Ordering::Relaxed)) {
                continue;
            }
            let mut captured = capture(&wic);
            for _ in 0..5 {
                if captured.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
                captured = capture(&wic);
            }
            let Some(captured) = captured else { continue };
            let now = Instant::now();
            if last.as_ref().is_some_and(|(print, at)| *print == captured.fingerprint && now - *at < Duration::from_millis(450)) {
                continue;
            }
            last = Some((captured.fingerprint, now));
            overlay::send(Cmd::Show(captured.pill));
            if let Some(entry) = captured.entry {
                let _ = entries.send(entry);
                win::post(host, WM_APP_CLIP);
            }
            if let Some((encoded, key)) = captured.deferred_png {
                let path = history::png_path(&key);
                if !path.exists() {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    if let Ok(frame) = wic.decode_memory(&encoded)
                        && wic.encode_png(&frame, &path).is_err()
                    {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    });
}

unsafe fn put_global(format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let Ok(global) = GlobalAlloc(GMEM_MOVEABLE, bytes.len()) else { return false };
        let ptr = GlobalLock(global) as *mut u8;
        if ptr.is_null() {
            let _ = GlobalFree(Some(global));
            return false;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(global);
        if SetClipboardData(format, Some(HANDLE(global.0))).is_err() {
            let _ = GlobalFree(Some(global));
            return false;
        }
        true
    }
}

fn utf16_bytes(text: &str, terminators: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2 + terminators * 2);
    for unit in text.encode_utf16() {
        bytes.extend(unit.to_le_bytes());
    }
    bytes.extend(std::iter::repeat_n(0u8, terminators * 2));
    bytes
}

fn files_payload(paths: &[&str]) -> Vec<u8> {
    let header = std::mem::size_of::<DROPFILES>();
    let mut bytes = vec![0u8; header];
    bytes[..4].copy_from_slice(&(header as u32).to_le_bytes());
    // fWide sits after pFiles (4) + pt (8) + fNC (4).
    bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
    for path in paths {
        bytes.extend(utf16_bytes(path, 1));
    }
    bytes.extend([0, 0]);
    bytes
}

pub fn restore(owner: HWND, entry: &Entry) -> bool {
    enum Payload {
        Text,
        Files,
        Image(Vec<u8>, Vec<u8>),
    }
    let mut payload = if entry.kind == Kind::Files { Payload::Files } else { Payload::Text };
    if entry.kind == Kind::Image {
        let png_file = entry.image.as_deref().map(history::png_path).filter(|p| p.exists());
        if let Some(path) = png_file {
            let decoded = Wic::new().and_then(|wic| wic.dib_from_file(&path.to_string_lossy()));
            if let (Ok(dib), Ok(png)) = (decoded, std::fs::read(&path)) {
                payload = Payload::Image(dib, png);
            }
        } else if Path::new(&*entry.full).exists() {
            payload = Payload::Files;
        } else {
            // Entry has no saved bitmap to restore.
            return false;
        }
    }

    let Some(open) = Open::acquire(Some(owner)) else { return false };
    let ok = unsafe {
        let _ = EmptyClipboard();
        match &payload {
            Payload::Text => put_global(CF_UNICODETEXT.0 as u32, &utf16_bytes(&entry.full, 1)),
            Payload::Files => {
                let paths: Vec<&str> = entry.full.lines().collect();
                put_global(CF_HDROP.0 as u32, &files_payload(&paths))
            }
            Payload::Image(dib, png) => {
                let placed = put_global(CF_DIB.0 as u32, dib);
                put_global(formats().png, png);
                placed
            }
        }
    };
    drop(open);
    OWN_WRITE_SEQ.store(unsafe { GetClipboardSequenceNumber() }, Ordering::Relaxed);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdrop_layout() {
        let bytes = files_payload(&["C:\\a", "C:\\b"]);
        let header = std::mem::size_of::<DROPFILES>();
        assert_eq!(header, 20);
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 20);
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
        // Two NUL-terminated paths and a final NUL.
        assert_eq!(bytes.len(), 20 + (5 * 2) * 2 + 2);
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
    }

    #[test]
    fn utf16_terminates() {
        assert_eq!(utf16_bytes("a", 1), vec![b'a', 0, 0, 0]);
    }
}
