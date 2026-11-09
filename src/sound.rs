use std::f64::consts::PI;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

pub const NAMES: [&str; 10] = [
    "Velvet Drop",
    "Silk Pluck",
    "Warm Bubble",
    "Zen Chime",
    "Celestial",
    "Marimba Tap",
    "Harp Gliss",
    "Acoustic Click",
    "Apple Tock",
    "Mute",
];
pub const MUTE: usize = 9;

const RATE: u32 = 44100;
const TAU: f64 = 2.0 * PI;

fn render(duration: f64, f: impl Fn(f64, f64, f64) -> f64) -> Vec<f64> {
    let n = (RATE as f64 * duration) as usize;
    let count = n as f64;
    (0..n).map(|i| f(i as f64, count, i as f64 / RATE as f64)).collect()
}

fn decay(i: f64, n: f64, shape: f64) -> f64 {
    (-i / (n * shape)).exp()
}

fn sine(freq: f64, t: f64) -> f64 {
    (TAU * freq * t).sin()
}

fn synth(sound: usize) -> Vec<f64> {
    match sound {
        0 => render(0.065, |i, n, t| sine(680.0 - 260.0 * (i / n), t) * decay(i, n, 0.28) * 0.75),
        1 => render(0.08, |i, n, t| (sine(880.0, t) + 0.35 * sine(1760.0, t)) * decay(i, n, 0.25) * 0.65),
        2 => render(0.05, |i, n, t| {
            let freq = 300.0 + 280.0 * (PI * 0.5 * (i / n)).sin();
            sine(freq, t) * (PI * (i / n)).sin() * 0.8
        }),
        3 => render(0.16, |i, n, t| (0.7 * sine(528.0, t) + 0.3 * sine(792.0, t)) * decay(i, n, 0.35) * 0.65),
        4 => render(0.18, |i, n, t| (0.5 * sine(1046.5, t) + 0.3 * sine(1567.98, t) + 0.2 * sine(2093.0, t)) * decay(i, n, 0.30) * 0.6),
        5 => render(0.07, |i, n, t| (sine(440.0, t) + 0.25 * sine(1320.0, t)) * decay(i, n, 0.20) * 0.7),
        6 => render(0.12, |i, n, t| {
            let voice = |freq: f64, start: f64| if t >= start { sine(freq, t) * (-(t - start) / 0.04).exp() } else { 0.0 };
            (voice(880.0, 0.0) * 0.4 + voice(1108.7, 0.02) * 0.35 + voice(1318.5, 0.04) * 0.35) * decay(i, n, 0.32) * 0.65
        }),
        7 => render(0.035, |i, n, t| (sine(1900.0, t) + 0.5 * sine(950.0, t)) * decay(i, n, 0.15) * 0.5),
        8 => render(0.04, |i, n, t| sine(1350.0 - 700.0 * (i / n), t) * decay(i, n, 0.18) * 0.7),
        _ => Vec::new(),
    }
}

fn wav(samples: &[f64], volume: u32) -> Vec<u8> {
    let gain = (volume as f64 / 100.0).clamp(0.0, 1.0);
    let data_len = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for s in samples {
        out.extend(((s * gain * 32767.0).clamp(-32767.0, 32767.0) as i16).to_le_bytes());
    }
    out
}

pub struct Player {
    key: Option<(usize, u32)>,
    buffer: Vec<u8>,
}

impl Player {
    pub fn new() -> Player {
        Player { key: None, buffer: Vec::new() }
    }

    pub fn play(&mut self, sound: usize, volume: u32) {
        if sound >= MUTE || volume == 0 {
            return;
        }
        unsafe {
            if self.key != Some((sound, volume)) {
                // PlaySound reads this buffer asynchronously.
                let _ = PlaySoundW(PCWSTR::null(), None, SND_ASYNC);
                self.buffer = wav(&synth(sound), volume);
                self.key = Some((sound, volume));
            }
            let _ = PlaySoundW(PCWSTR(self.buffer.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sound_is_well_formed() {
        for (sound, name) in NAMES.iter().enumerate().take(MUTE) {
            let samples = synth(sound);
            assert!(!samples.is_empty(), "{name}");
            assert!(samples.iter().all(|s| s.abs() <= 1.0), "{name} clips");
            let bytes = wav(&samples, 65);
            assert_eq!(bytes.len(), 44 + samples.len() * 2);
            assert_eq!(&bytes[..4], b"RIFF");
        }
    }

    #[test]
    fn volume_scales_output() {
        let samples = synth(0);
        let peak = |volume| {
            wav(&samples, volume)[44..].as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs()).max().unwrap()
        };
        assert_eq!(peak(0), 0);
        assert!(peak(100) > peak(20));
    }
}
