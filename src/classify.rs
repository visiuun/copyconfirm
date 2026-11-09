#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Text,
    Link,
    Email,
    Code,
    Color,
    Files,
    Image,
    Private,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Link => "link",
            Kind::Email => "email",
            Kind::Code => "code",
            Kind::Color => "color",
            Kind::Files => "files",
            Kind::Image => "image",
            Kind::Private => "private",
        }
    }

    pub fn from_key(key: &str) -> Kind {
        match key {
            "link" => Kind::Link,
            "email" => Kind::Email,
            "code" => Kind::Code,
            "color" => Kind::Color,
            "files" => Kind::Files,
            "image" => Kind::Image,
            _ => Kind::Text,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Link => "Link",
            Kind::Email => "Email",
            Kind::Code => "Code",
            Kind::Color => "Color",
            Kind::Files => "Files",
            Kind::Image => "Image",
            Kind::Private => "Private",
        }
    }
}

pub fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn preview_line(cleaned: &str) -> String {
    let first = cleaned.lines().next().unwrap_or("");
    let mut out = String::with_capacity(first.len().min(320));
    for word in first.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    match out.char_indices().nth(80) {
        Some((end, _)) => out[..end].to_string(),
        None => out,
    }
}

pub fn parse_color(text: &str) -> Option<[u8; 3]> {
    let t = text.trim();
    if t.len() > 35 {
        return None;
    }
    if let Some(hex) = t.strip_prefix('#') {
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let nib = |i: usize| u8::from_str_radix(&hex[i..i + 1], 16).ok();
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return match hex.len() {
            3 => Some([nib(0)? * 17, nib(1)? * 17, nib(2)? * 17]),
            6 | 8 => Some([byte(0)?, byte(2)?, byte(4)?]),
            _ => None,
        };
    }
    let lower = t.to_ascii_lowercase();
    let inner = lower.strip_prefix("rgba").or_else(|| lower.strip_prefix("rgb"))?.trim_start();
    let inner = inner.strip_prefix('(')?.strip_suffix(')')?;
    let mut parts = inner.split(',').map(str::trim);
    let mut channel = || parts.next()?.parse::<u8>().ok();
    let color = [channel()?, channel()?, channel()?];
    match parts.next() {
        None => Some(color),
        Some(alpha) if alpha.parse::<f32>().is_ok() && parts.next().is_none() => Some(color),
        _ => None,
    }
}

fn has_whitespace(s: &str) -> bool {
    s.chars().any(char::is_whitespace)
}

pub fn is_link(s: &str) -> bool {
    if s.is_empty() || has_whitespace(s) {
        return false;
    }
    let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return s.len() > if lower.starts_with("https") { 8 } else { 7 };
    }
    if let Some(rest) = lower.strip_prefix("www.") {
        return !rest.is_empty() && s[4..].contains('.');
    }
    // bare-domain.tld/path
    let Some((host, _)) = s.split_once('/') else { return false };
    let Some((name, tld)) = host.rsplit_once('.') else { return false };
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && tld.len() >= 2
        && tld.bytes().all(|b| b.is_ascii_alphabetic())
}

pub fn is_email(s: &str) -> bool {
    if s.len() > 254 || has_whitespace(s) {
        return false;
    }
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else { return false };
    let Some((name, tld)) = domain.rsplit_once('.') else { return false };
    !local.is_empty() && !name.is_empty() && tld.len() >= 2 && !tld.contains('/')
}

fn after<'a>(line: &'a str, keyword: &str) -> Option<&'a str> {
    line.strip_prefix(keyword).filter(|rest| rest.starts_with(' ')).map(str::trim_start)
}

fn line_is_code(raw: &str) -> bool {
    let line = raw.trim();
    if line.len() < 2 {
        return false;
    }
    if ["};", "=>", "();", "#include", "#!/", "</", "/>", "<div", "!=", "==", "+="].iter().any(|m| line.contains(m)) {
        return true;
    }
    if line.ends_with(';') || line.ends_with('{') {
        return true;
    }
    for kw in ["def", "fn", "func", "function"] {
        if after(line, kw).is_some_and(|r| r.contains('(')) {
            return true;
        }
    }
    if line.starts_with("function(") {
        return true;
    }
    for kw in ["const", "let", "var", "auto", "int", "pub"] {
        if after(line, kw).is_some_and(|r| r.contains('=') || r.contains('(')) {
            return true;
        }
    }
    if after(line, "class").or_else(|| after(line, "struct")).is_some_and(|r| r.ends_with(':') || r.contains('(') || r.ends_with('{')) {
        return true;
    }
    if let Some(r) = after(line, "import") {
        let words = r.split_whitespace().count();
        if words == 1 || r.contains(" from ") || r.contains(" as ") || r.starts_with('{') || r.starts_with('*') {
            return true;
        }
    }
    if after(line, "from").is_some_and(|r| r.contains(" import ")) {
        return true;
    }
    line.starts_with("SELECT ") && line.contains(" FROM ")
}

pub fn is_code(sample: &str) -> bool {
    sample.lines().take(12).any(line_is_code)
}

pub struct Classified {
    pub kind: Kind,
    pub preview: String,
    pub badge: String,
}

pub fn classify(text: &str, total_chars: usize) -> Classified {
    let cleaned = text.trim();
    let preview = preview_line(cleaned);
    if parse_color(cleaned).is_some() {
        return Classified { kind: Kind::Color, preview, badge: "COLOR".into() };
    }
    if is_link(cleaned) {
        return Classified { kind: Kind::Link, preview, badge: "LINK".into() };
    }
    if is_email(cleaned) {
        return Classified { kind: Kind::Email, preview, badge: "EMAIL".into() };
    }
    let sample_end = cleaned.char_indices().nth(600).map_or(cleaned.len(), |(i, _)| i);
    if is_code(&cleaned[..sample_end]) {
        return Classified { kind: Kind::Code, preview, badge: format!("{}c code", grouped(total_chars)) };
    }
    let badge = if total_chars < 500 {
        format!("{}w \u{2022} {}c", cleaned.split_whitespace().count(), total_chars)
    } else {
        format!("{} chars", grouped(total_chars))
    };
    Classified { kind: Kind::Text, preview, badge }
}

pub fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if needle_lower.is_ascii() {
        let n = needle_lower.as_bytes();
        return hay.as_bytes().windows(n.len()).any(|w| w.eq_ignore_ascii_case(n));
    }
    hay.to_lowercase().contains(needle_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#1DB954"), Some([0x1D, 0xB9, 0x54]));
        assert_eq!(parse_color(" #abc "), Some([0xAA, 0xBB, 0xCC]));
        assert_eq!(parse_color("#11223344"), Some([0x11, 0x22, 0x33]));
        assert_eq!(parse_color("RGB( 1, 2 ,3 )"), Some([1, 2, 3]));
        assert_eq!(parse_color("rgba(9,8,7,0.5)"), Some([9, 8, 7]));
        for no in ["#12", "#xyz", "rgb(300,0,0)", "rgb(1,2)", "#é12", "red", ""] {
            assert_eq!(parse_color(no), None, "{no}");
        }
    }

    #[test]
    fn links() {
        for yes in ["https://example.com/a?b=1", "HTTP://x.io", "www.example.com", "github.com/visiuun"] {
            assert!(is_link(yes), "{yes}");
        }
        for no in ["https://example.com is great", "see www.x.com", "www.", "and/or", "1.5/2", "https://"] {
            assert!(!is_link(no), "{no}");
        }
    }

    #[test]
    fn emails() {
        assert!(is_email("first.last@mail.example.org"));
        for no in ["a@b", "a b@c.de", "@x.io", "a@b@c.io", "https://x.io/@me"] {
            assert!(!is_email(no), "{no}");
        }
    }

    #[test]
    fn code_detection() {
        for yes in [
            "fn main() {",
            "def run(x):",
            "const a = 1",
            "let total = a + b",
            "import os",
            "import { useState } from 'react'",
            "from a import b",
            "class Foo:",
            "SELECT id FROM users",
            "<div>hi</div>",
            "const client = new Client({ intents });",
        ] {
            assert!(is_code(yes), "{yes}");
        }
        for no in [
            "let me know when you're free",
            "the class was great today",
            "import duties rose sharply last year",
            "var is short for variance",
            "function follows form",
            "See you at 5pm: bring snacks",
        ] {
            assert!(!is_code(no), "{no}");
        }
    }

    #[test]
    fn badges_and_previews() {
        let c = classify("  hello   world\nsecond line", 26);
        assert_eq!(c.kind, Kind::Text);
        assert_eq!(c.preview, "hello world");
        assert_eq!(c.badge, "4w \u{2022} 26c");
        assert_eq!(classify("plain", 1234567).badge, "1,234,567 chars");
        assert_eq!(classify("#fff", 4).kind, Kind::Color);
    }

    #[test]
    fn preview_cap_respects_multibyte() {
        let long = "é".repeat(200);
        assert_eq!(preview_line(&long).chars().count(), 80);
    }

    #[test]
    fn search_is_case_insensitive() {
        assert!(contains_ci("Hello World", "lo wo"));
        assert!(contains_ci("ÉCOLE", "école"));
        assert!(!contains_ci("abc", "abcd"));
    }

    #[test]
    fn grouping() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
    }
}
