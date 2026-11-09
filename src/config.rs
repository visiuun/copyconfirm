use crate::json::{J, quote};
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq)]
pub enum Anchor {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 6] =
        [Anchor::TopLeft, Anchor::TopCenter, Anchor::TopRight, Anchor::BottomLeft, Anchor::BottomCenter, Anchor::BottomRight];

    pub fn name(self) -> &'static str {
        match self {
            Anchor::TopLeft => "Top Left",
            Anchor::TopCenter => "Top Center",
            Anchor::TopRight => "Top Right",
            Anchor::BottomLeft => "Bottom Left",
            Anchor::BottomCenter => "Bottom Center",
            Anchor::BottomRight => "Bottom Right",
        }
    }

    pub fn is_bottom(self) -> bool {
        matches!(self, Anchor::BottomLeft | Anchor::BottomCenter | Anchor::BottomRight)
    }

    /// 0 = left, 1 = center, 2 = right.
    pub fn column(self) -> u8 {
        match self {
            Anchor::TopLeft | Anchor::BottomLeft => 0,
            Anchor::TopCenter | Anchor::BottomCenter => 1,
            Anchor::TopRight | Anchor::BottomRight => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Config {
    pub popup_duration: f32,
    pub position: Anchor,
    pub screen_margin: i32,
    pub scale: f32,
    pub font_scale: f32,
    pub opacity: f32,
    pub click_through: bool,
    pub sound: usize,
    pub sound_volume: u32,
    pub stack_limit: usize,
    pub show_char_count: bool,
    pub show_icon: bool,
    pub max_preview_length: usize,
    pub detect_colors: bool,
    pub detect_images: bool,
    pub follow_cursor: bool,
    pub keep_history: bool,
    pub paused: bool,
    pub show_tray: bool,
}

pub const DEFAULT: Config = Config {
    popup_duration: 2.4,
    position: Anchor::BottomCenter,
    screen_margin: 45,
    scale: 1.0,
    font_scale: 1.0,
    opacity: 0.98,
    click_through: false,
    sound: 0,
    sound_volume: 65,
    stack_limit: 4,
    show_char_count: true,
    show_icon: true,
    max_preview_length: 38,
    detect_colors: true,
    detect_images: true,
    follow_cursor: true,
    keep_history: true,
    paused: false,
    show_tray: true,
};

static CURRENT: Mutex<Config> = Mutex::new(DEFAULT);

pub fn get() -> Config {
    *CURRENT.lock().unwrap()
}

pub fn set(config: Config) {
    *CURRENT.lock().unwrap() = config;
}

pub fn data_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(dir) = std::env::var_os("COPYHUD_DIR") {
            return dir.into();
        }
        let base = std::env::var_os("APPDATA").or_else(|| std::env::var_os("USERPROFILE")).unwrap_or_default();
        PathBuf::from(base).join("OledCopyHUD")
    })
}

fn path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn parse(text: &str) -> Config {
    let d = DEFAULT;
    let Some(j) = J::parse(text) else { return d };
    let f = |k: &str, dv: f32, lo: f32, hi: f32| {
        let v = j.num(k).map_or(dv, |v| v as f32);
        if v.is_finite() { v.clamp(lo, hi) } else { dv }
    };
    let i = |k: &str, dv: i32, lo: i32, hi: i32| j.num(k).map_or(dv, |v| v as i32).clamp(lo, hi);
    let b = |k: &str, dv: bool| j.bool(k).unwrap_or(dv);
    let position_name = j.str("position_mode").unwrap_or("");
    let sound_name = j.str("sound_feedback").unwrap_or("");
    Config {
        popup_duration: f("popup_duration", d.popup_duration, 0.5, 10.0),
        position: Anchor::ALL.into_iter().find(|a| a.name() == position_name).unwrap_or(d.position),
        screen_margin: i("screen_margin", d.screen_margin, 10, 300),
        scale: f("scale", d.scale, 0.7, 1.7),
        font_scale: f("font_scale", d.font_scale, 0.7, 1.5),
        opacity: f("opacity", d.opacity, 0.3, 1.0),
        click_through: b("click_through", d.click_through),
        sound: crate::sound::NAMES.iter().position(|n| *n == sound_name).unwrap_or(d.sound),
        sound_volume: i("sound_volume", d.sound_volume as i32, 0, 100) as u32,
        stack_limit: i("stack_limit", d.stack_limit as i32, 1, 6) as usize,
        show_char_count: b("show_char_count", d.show_char_count),
        show_icon: b("show_icon", d.show_icon),
        max_preview_length: i("max_preview_length", d.max_preview_length as i32, 12, 120) as usize,
        detect_colors: b("detect_colors", d.detect_colors),
        detect_images: b("detect_images", d.detect_images),
        follow_cursor: b("follow_cursor", d.follow_cursor),
        keep_history: b("keep_history", d.keep_history),
        paused: b("paused", d.paused),
        show_tray: b("show_tray", d.show_tray),
    }
}

pub fn serialize(c: &Config) -> String {
    let mut o = String::with_capacity(512);
    o.push_str("{\n");
    let _ = writeln!(o, "    \"popup_duration\": {},", c.popup_duration);
    o.push_str("    \"position_mode\": ");
    quote(&mut o, c.position.name());
    o.push_str(",\n");
    let _ = writeln!(o, "    \"screen_margin\": {},", c.screen_margin);
    let _ = writeln!(o, "    \"scale\": {},", c.scale);
    let _ = writeln!(o, "    \"font_scale\": {},", c.font_scale);
    let _ = writeln!(o, "    \"opacity\": {},", c.opacity);
    let _ = writeln!(o, "    \"click_through\": {},", c.click_through);
    o.push_str("    \"sound_feedback\": ");
    quote(&mut o, crate::sound::NAMES[c.sound]);
    o.push_str(",\n");
    let _ = writeln!(o, "    \"sound_volume\": {},", c.sound_volume);
    let _ = writeln!(o, "    \"stack_limit\": {},", c.stack_limit);
    let _ = writeln!(o, "    \"show_char_count\": {},", c.show_char_count);
    let _ = writeln!(o, "    \"show_icon\": {},", c.show_icon);
    let _ = writeln!(o, "    \"max_preview_length\": {},", c.max_preview_length);
    let _ = writeln!(o, "    \"detect_colors\": {},", c.detect_colors);
    let _ = writeln!(o, "    \"detect_images\": {},", c.detect_images);
    let _ = writeln!(o, "    \"follow_cursor\": {},", c.follow_cursor);
    let _ = writeln!(o, "    \"keep_history\": {},", c.keep_history);
    let _ = writeln!(o, "    \"paused\": {},", c.paused);
    let _ = writeln!(o, "    \"show_tray\": {}", c.show_tray);
    o.push_str("}\n");
    o
}

pub fn load() {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    set(parse(&text));
}

pub fn save() {
    let text = serialize(&get());
    write_atomic(&path(), text.as_bytes());
}

pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_existing_python_config() {
        let c = parse(
            r#"{"popup_duration": 1.3599999999999994, "position_mode": "Bottom Center", "screen_margin": 66,
            "scale": 1.0, "click_through": true, "sound_feedback": "Velvet Drop", "sound_volume": 6,
            "stack_limit": 3, "frosted_glass": false}"#,
        );
        assert!((c.popup_duration - 1.36).abs() < 0.001);
        assert_eq!(c.screen_margin, 66);
        assert!(c.click_through);
        assert_eq!(c.sound_volume, 6);
        assert_eq!(c.stack_limit, 3);
    }

    #[test]
    fn repairs_bad_values() {
        let c = parse(r#"{"scale": 99, "stack_limit": -4, "position_mode": "Nowhere", "sound_feedback": 7}"#);
        assert_eq!(c.scale, 1.7);
        assert_eq!(c.stack_limit, 1);
        assert!(c.position == DEFAULT.position);
        assert_eq!(c.sound, 0);
    }

    #[test]
    fn round_trips() {
        let mut c = DEFAULT;
        c.position = Anchor::TopLeft;
        c.sound = 4;
        c.paused = true;
        c.show_tray = false;
        assert!(parse(&serialize(&c)) == c);
    }
}
