use crate::win::WM_APP_QUIT;
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile};
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW};
use windows::core::{HSTRING, Interface, PCWSTR, w};

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("CopyHUD");

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
}

fn register_autostart(exe: &Path) {
    let command: Vec<u16> = format!("\"{}\" --tray", exe.display()).encode_utf16().chain([0]).collect();
    unsafe {
        let _ = RegSetKeyValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            RUN_VALUE,
            REG_SZ.0,
            Some(command.as_ptr() as *const _),
            (command.len() * 2) as u32,
        );
    }
}

pub fn set_autostart(enabled: bool) {
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            register_autostart(&exe);
        }
    } else {
        unsafe {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

fn create_shortcut(exe: &Path) {
    let Some(appdata) = std::env::var_os("APPDATA") else { return };
    let link_path = PathBuf::from(appdata).join("Microsoft\\Windows\\Start Menu\\Programs\\Copy HUD.lnk");
    unsafe {
        let Ok(link) = CoCreateInstance::<_, IShellLinkW>(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return };
        let _ = link.SetPath(&HSTRING::from(exe.as_os_str()));
        let _ = link.SetDescription(w!("Copy HUD"));
        if let Ok(file) = link.cast::<IPersistFile>() {
            let _ = file.Save(&HSTRING::from(link_path.as_os_str()), true);
        }
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

fn replace_exe(source: &Path, target: &Path) -> bool {
    if let Some(dir) = target.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // The previous version needs a moment to exit and release its file.
    for _ in 0..50 {
        if std::fs::copy(source, target).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Installs or upgrades into %LOCALAPPDATA%\copyconfirm and launches that copy.
/// Returns true when this process should exit.
pub fn hand_off(host_class: PCWSTR, args: &[String]) -> bool {
    if args.iter().any(|a| a == "--portable") {
        return false;
    }
    let (Ok(exe), Some(base)) = (std::env::current_exe(), std::env::var_os("LOCALAPPDATA")) else { return false };
    let target = PathBuf::from(base).join("copyconfirm").join("copyconfirm.exe");
    if same_path(&exe, &target) || exe.components().any(|c| c.as_os_str() == "target") {
        return false;
    }

    let first_install = !target.exists();
    let unchanged = !first_install && std::fs::read(&exe).ok() == std::fs::read(&target).ok();
    if !unchanged {
        unsafe {
            if let Ok(running) = FindWindowW(host_class, PCWSTR::null()) {
                let _ = PostMessageW(Some(running), WM_APP_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if !replace_exe(&exe, &target) {
            return false;
        }
        create_shortcut(&target);
        if first_install && !autostart_enabled() {
            register_autostart(&target);
        }
    }

    let mut launch = std::process::Command::new(&target);
    if first_install {
        launch.arg("--open");
    }
    launch.spawn().is_ok()
}
