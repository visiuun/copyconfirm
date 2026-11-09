use crate::classify::Kind;
use crate::config::{data_dir, write_atomic};
use crate::imaging::Thumb;
use crate::json::{J, quote};
use std::fmt::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub const CAPACITY: usize = 50;

#[derive(Clone)]
pub struct Entry {
    pub kind: Kind,
    pub preview: Arc<str>,
    pub badge: Arc<str>,
    pub full: Arc<str>,
    pub timestamp: f64,
    pub id: Arc<str>,
    /// Key of the files in `images/` (thumbnail, and PNG for bitmap copies).
    pub image: Option<Arc<str>>,
}

impl Entry {
    fn same_content(&self, other: &Entry) -> bool {
        self.kind == other.kind && self.image == other.image && self.full == other.full
    }
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub fn new_id() -> Arc<str> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{nanos:x}").into()
}

fn images_dir() -> PathBuf {
    data_dir().join("images")
}

pub fn png_path(key: &str) -> PathBuf {
    images_dir().join(format!("{key}.png"))
}

fn thumb_path(key: &str) -> PathBuf {
    images_dir().join(format!("{key}.thumb"))
}

pub fn save_thumb(key: &str, thumb: &Thumb) {
    let mut bytes = Vec::with_capacity(8 + thumb.pixels.len());
    bytes.extend(thumb.width.to_le_bytes());
    bytes.extend(thumb.height.to_le_bytes());
    bytes.extend_from_slice(&thumb.pixels);
    write_atomic(&thumb_path(key), &bytes);
}

pub fn load_thumb(key: &str) -> Option<Thumb> {
    let bytes = std::fs::read(thumb_path(key)).ok()?;
    let width = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?);
    let height = u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?);
    let pixels = bytes.get(8..)?;
    (pixels.len() as u64 == width as u64 * height as u64 * 4).then(|| Thumb { width, height, pixels: pixels.to_vec() })
}

fn remove_image(key: &str) {
    let _ = std::fs::remove_file(png_path(key));
    let _ = std::fs::remove_file(thumb_path(key));
}

pub fn purge_images() {
    let _ = std::fs::remove_dir_all(images_dir());
}

pub struct History {
    pub items: Vec<Entry>,
}

static WRITE_LOCK: Mutex<()> = Mutex::new(());

impl History {
    pub fn load() -> History {
        let text = std::fs::read_to_string(data_dir().join("history.json")).unwrap_or_default();
        History { items: parse(&text) }
    }

    pub fn add(&mut self, entry: Entry) {
        if let Some(at) = self.items.iter().position(|e| e.same_content(&entry)) {
            self.items.remove(at);
        }
        self.items.insert(0, entry);
        while self.items.len() > CAPACITY {
            let gone = self.items.pop().unwrap();
            self.release(&gone);
        }
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            let gone = self.items.remove(index);
            self.release(&gone);
        }
    }

    pub fn clear(&mut self) {
        for gone in std::mem::take(&mut self.items) {
            self.release(&gone);
        }
    }

    fn release(&self, gone: &Entry) {
        if let Some(key) = &gone.image
            && !self.items.iter().any(|e| e.image.as_ref() == Some(key))
        {
            remove_image(key);
        }
    }

    pub fn save_async(&self) {
        let snapshot = if crate::config::get().keep_history { self.items.clone() } else { Vec::new() };
        let _ = std::thread::Builder::new().spawn(move || write_file(&snapshot));
    }

    pub fn save_now(&self) {
        if crate::config::get().keep_history {
            write_file(&self.items);
        } else {
            write_file(&[]);
        }
    }
}

fn write_file(items: &[Entry]) {
    let text = serialize(items);
    let _guard = WRITE_LOCK.lock().unwrap();
    write_atomic(&data_dir().join("history.json"), text.as_bytes());
}

pub fn serialize(items: &[Entry]) -> String {
    let mut o = String::with_capacity(items.iter().map(|e| e.full.len() + 160).sum::<usize>() + 8);
    o.push('[');
    for (i, e) in items.iter().enumerate() {
        o.push_str(if i == 0 { "\n  {\"type\": " } else { ",\n  {\"type\": " });
        quote(&mut o, e.kind.key());
        o.push_str(", \"preview\": ");
        quote(&mut o, &e.preview);
        o.push_str(", \"badge\": ");
        quote(&mut o, &e.badge);
        o.push_str(", \"full_text\": ");
        quote(&mut o, &e.full);
        let _ = write!(o, ", \"timestamp\": {}, \"id\": ", e.timestamp);
        quote(&mut o, &e.id);
        if let Some(key) = &e.image {
            o.push_str(", \"image\": ");
            quote(&mut o, key);
        }
        o.push('}');
    }
    o.push_str("\n]\n");
    o
}

pub fn parse(text: &str) -> Vec<Entry> {
    let Some(J::Arr(rows)) = J::parse(text) else { return Vec::new() };
    let clean = |s: &str| -> Arc<str> { s.replace('\0', "").into() };
    rows.iter()
        .filter_map(|row| {
            Some(Entry {
                kind: Kind::from_key(row.str("type").unwrap_or("text")),
                preview: clean(row.str("preview")?),
                badge: clean(row.str("badge").unwrap_or("")),
                full: clean(row.str("full_text").unwrap_or("")),
                timestamp: row.num("timestamp").unwrap_or(0.0),
                id: row.str("id").map_or_else(new_id, Arc::from),
                image: row.str("image").map(Arc::from),
            })
        })
        .take(CAPACITY)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(text: &str) -> Entry {
        Entry { kind: Kind::Text, preview: text.into(), badge: "b".into(), full: text.into(), timestamp: 1.5, id: text.into(), image: None }
    }

    #[test]
    fn duplicates_move_to_front() {
        let mut h = History { items: Vec::new() };
        h.add(entry("a"));
        h.add(entry("b"));
        h.add(entry("a"));
        assert_eq!(h.items.len(), 2);
        assert_eq!(&*h.items[0].full, "a");
    }

    #[test]
    fn capacity_is_enforced() {
        let mut h = History { items: Vec::new() };
        for i in 0..CAPACITY + 10 {
            h.add(entry(&i.to_string()));
        }
        assert_eq!(h.items.len(), CAPACITY);
        assert_eq!(&*h.items[0].full, (CAPACITY + 9).to_string());
    }

    #[test]
    fn same_sized_images_stay_distinct() {
        let mut h = History { items: Vec::new() };
        let mut a = entry("[Image 10x10]");
        a.kind = Kind::Image;
        a.image = Some("one".into());
        let mut b = a.clone();
        b.image = Some("two".into());
        h.add(a);
        h.add(b);
        assert_eq!(h.items.len(), 2);
    }

    #[test]
    fn reads_python_history_and_strips_nuls() {
        let rows = parse(
            r#"[{"type":"text","preview":"x\u0000","badge":"3w • 78c","full_text":"x","timestamp":1794189203.17,"id":"1_2"},
                {"no_preview":true}]"#,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(&*rows[0].preview, "x");
    }

    #[test]
    fn round_trips() {
        let mut e = entry("multi\nline \"q\" \u{1F600}");
        e.image = Some("k".into());
        let back = parse(&serialize(std::slice::from_ref(&e)));
        assert_eq!(back[0].full, e.full);
        assert_eq!(back[0].image, e.image);
    }
}
