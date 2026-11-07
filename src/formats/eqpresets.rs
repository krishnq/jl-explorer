//! Music EQ presets for small full-range drivers, and an approximate response model.
//!
//! Presets are offsets applied on top of the speaker's stock tuning (which already
//! compensates the driver). Band layout of the 10-band music EQ:
//!   0: low shelf 120 Hz (bass knob)   1: high-pass (driver protection)
//!   2..8: peaking at the stock centres (~100, 220, 450, 1k, 2.5k, 5k, 10k Hz)
//!   9: low-pass near 20 kHz
//! Safety rules: the high-pass always stays on, nothing is boosted below ~100 Hz,
//! and the global gain is set to cancel the largest boost so loud passages don't clip.

use super::eqcfg::{Band, EqTable};

pub const SAMPLE_RATE: f64 = 44100.0;
const STD_CENTRES: [i32; 7] = [100, 220, 450, 1000, 2500, 5000, 10000];
const HIGH_PASS: u16 = 0;
const LOW_PASS: u16 = 1;
const PEAKING: u16 = 2;
const HIGH_SHELF: u16 = 3;
const LOW_SHELF: u16 = 4;

pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    /// Gain of the 120 Hz low shelf (dB).
    pub bass_shelf: f32,
    /// Offsets for the 7 peaking bands at ~100, 220, 450, 1k, 2.5k, 5k, 10k Hz (dB).
    pub bands: [f32; 7],
    pub high_pass_hz: i32,
    /// false: ignore the stock tuning (raw driver, for experiments).
    pub on_stock: bool,
}

const fn p(name: &'static str, description: &'static str, bass_shelf: f32, bands: [f32; 7], high_pass_hz: i32) -> Preset {
    Preset {
        name,
        description,
        bass_shelf,
        bands,
        high_pass_hz,
        on_stock: true,
    }
}

pub static PRESETS: &[Preset] = &[
    p("Stock", "The factory tuning, unchanged.", 0.0, [0.0; 7], 52),
    p(
        "Bass Boost",
        "Fuller low end, kept above the range the driver can't play.",
        2.5,
        [1.5, 0.5, 0.0, 0.0, 0.0, 0.0, 0.0],
        55,
    ),
    p(
        "Bass Reducer",
        "Less boom; good for small rooms, desks, or corners.",
        -2.0,
        [-1.5, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        60,
    ),
    p(
        "Treble Boost",
        "More detail and sparkle.",
        0.0,
        [0.0, 0.0, 0.0, 0.0, 1.0, 2.5, 3.0],
        52,
    ),
    p(
        "Treble Reducer",
        "Softer highs; tames harsh or sibilant recordings.",
        0.0,
        [0.0, 0.0, 0.0, 0.0, -1.0, -2.5, -3.0],
        52,
    ),
    p(
        "Vocal / Podcast",
        "Clear speech: less rumble and boom, more presence.",
        -1.5,
        [-2.0, -1.0, -0.5, 1.0, 3.0, 2.0, 0.0],
        80,
    ),
    p(
        "Movie / Dialogue",
        "Dialogue forward without losing body.",
        0.5,
        [0.5, -0.5, -1.0, 1.0, 2.5, 1.5, 0.5],
        60,
    ),
    p(
        "Rock",
        "Classic V: punchy lows, recessed mids, bright guitars and cymbals.",
        1.0,
        [2.0, 0.5, -1.0, -1.0, 1.0, 2.5, 2.0],
        55,
    ),
    p(
        "Pop",
        "Gentle lift in warmth and clarity.",
        0.5,
        [1.0, 1.5, 0.5, 0.0, 1.0, 1.5, 1.0],
        55,
    ),
    p(
        "Jazz",
        "Warm and smooth with a little air.",
        0.5,
        [1.5, 1.0, 0.0, -0.5, 0.5, 1.0, 1.5],
        55,
    ),
    p(
        "Classical",
        "Natural balance, slight lift at the extremes.",
        0.5,
        [1.0, 0.0, 0.0, 0.0, -0.5, 0.5, 2.0],
        52,
    ),
    p(
        "Acoustic",
        "Body and string detail for guitars and voices.",
        0.0,
        [0.5, 2.0, 1.0, 0.0, 1.0, 1.0, 1.5],
        55,
    ),
    p(
        "Electronic / Dance",
        "Strong bass and crisp highs.",
        2.0,
        [2.0, 0.0, -1.0, -0.5, 0.5, 2.0, 2.5],
        55,
    ),
    p(
        "Hip-Hop",
        "Bass and kick forward, clean vocals.",
        2.5,
        [2.0, 0.5, -1.0, -0.5, 1.0, 0.5, 0.0],
        55,
    ),
    p(
        "Loudness (low volume)",
        "Adds back bass and treble the ear loses at low volume.",
        2.5,
        [1.0, 0.0, 0.0, -0.5, 0.0, 1.0, 2.5],
        55,
    ),
    Preset {
        name: "Raw driver (no tuning)",
        description: "All bands flat, only the protective high-pass. For comparison only.",
        bass_shelf: 0.0,
        bands: [0.0; 7],
        high_pass_hz: 52,
        on_stock: false,
    },
];

pub fn find(name: &str) -> Option<&'static Preset> {
    let key = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    PRESETS
        .iter()
        .find(|p| key(p.name) == key(name) || key(p.name).starts_with(&key(name)))
}

/// Whether a table has the stock 10-band layout (high-pass at 1, low-pass at 9).
fn stock_layout(t: &EqTable) -> bool {
    t.bands.len() == 10 && t.bands[1].filter == HIGH_PASS && t.bands[9].filter == LOW_PASS
}

/// Build the music EQ for `preset`, using `stock` (the original table) as the base.
pub fn apply(stock: &EqTable, preset: &Preset) -> EqTable {
    if std::ptr::eq(preset, &PRESETS[0]) {
        return stock.clone(); // factory tuning exactly as shipped
    }
    let use_stock = preset.on_stock && stock_layout(stock);
    let mut bands = Vec::with_capacity(10);
    bands.push(Band {
        index: 0,
        filter: LOW_SHELF,
        freq: 120,
        gain: preset.bass_shelf,
        q: 0.7,
    });
    bands.push(Band {
        index: 1,
        filter: HIGH_PASS,
        freq: preset.high_pass_hz,
        gain: 0.0,
        q: 0.7,
    });
    for (i, (&centre, &offset)) in STD_CENTRES.iter().zip(&preset.bands).enumerate() {
        let (freq, base, q) = if use_stock {
            let b = &stock.bands[i + 2];
            (b.freq, b.gain, b.q)
        } else {
            (centre, 0.0, 0.9)
        };
        bands.push(Band {
            index: (i + 2) as u16,
            filter: PEAKING,
            freq,
            gain: (base + offset).clamp(-12.0, 12.0),
            q,
        });
    }
    let lp = if stock_layout(stock) { stock.bands[9].freq } else { 20000 };
    bands.push(Band {
        index: 9,
        filter: LOW_PASS,
        freq: lp,
        gain: 0.0,
        q: 0.7,
    });
    let mut t = EqTable {
        global_gain: 0.0,
        enable_mask: 0x3FF,
        bands,
    };
    t.global_gain = headroom_gain(&t, stock);
    t
}

/// Highest point of the response (dB), ignoring the table's own global gain.
pub fn peak_db(t: &EqTable) -> f64 {
    let flat = EqTable {
        global_gain: 0.0,
        ..t.clone()
    };
    log_freqs(20.0, 20000.0, 400)
        .map(|f| response_db(&flat, f))
        .fold(f64::MIN, f64::max)
}

/// Global gain for `t` so that its overall peak (global gain included) is never above
/// the stock tuning's, which is known to be safe on this driver. Starts from the stock
/// global gain and lowers it by any extra boost, rounded down to 0.5 dB.
pub fn headroom_gain(t: &EqTable, stock: &EqTable) -> f32 {
    let excess = (peak_db(t) - peak_db(stock)).max(0.0);
    let g = stock.global_gain - ((excess * 2.0).ceil() / 2.0) as f32;
    if g == 0.0 {
        0.0
    } else {
        g
    } // no "-0.0"
}

pub fn log_freqs(lo: f64, hi: f64, n: usize) -> impl Iterator<Item = f64> {
    (0..n).map(move |i| lo * (hi / lo).powf(i as f64 / (n - 1) as f64))
}

/// Approximate magnitude response (dB) of the whole EQ at `f` Hz, using standard
/// RBJ biquads. The chip's own filter design may differ slightly.
pub fn response_db(t: &EqTable, f: f64) -> f64 {
    let mut db = t.global_gain as f64;
    for (i, b) in t.bands.iter().enumerate() {
        if t.enable_mask & (1 << i) == 0 {
            continue;
        }
        db += biquad_db(b, f);
    }
    db
}

fn biquad_db(b: &Band, f: f64) -> f64 {
    let fs = SAMPLE_RATE;
    let f0 = (b.freq as f64).clamp(1.0, fs / 2.0 - 1.0);
    let q = (b.q as f64).max(0.05);
    let a = 10f64.powf(b.gain as f64 / 40.0);
    let w0 = 2.0 * std::f64::consts::PI * f0 / fs;
    let (cw, sw) = (w0.cos(), w0.sin());
    let alpha = sw / (2.0 * q);
    let sa = 2.0 * a.sqrt() * alpha;
    let (b0, b1, b2, a0, a1, a2) = match b.filter {
        HIGH_PASS => ((1.0 + cw) / 2.0, -(1.0 + cw), (1.0 + cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        LOW_PASS => ((1.0 - cw) / 2.0, 1.0 - cw, (1.0 - cw) / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha),
        PEAKING => (
            1.0 + alpha * a,
            -2.0 * cw,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cw,
            1.0 - alpha / a,
        ),
        LOW_SHELF => (
            a * ((a + 1.0) - (a - 1.0) * cw + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
            a * ((a + 1.0) - (a - 1.0) * cw - sa),
            (a + 1.0) + (a - 1.0) * cw + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * cw),
            (a + 1.0) + (a - 1.0) * cw - sa,
        ),
        HIGH_SHELF => (
            a * ((a + 1.0) + (a - 1.0) * cw + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
            a * ((a + 1.0) + (a - 1.0) * cw - sa),
            (a + 1.0) - (a - 1.0) * cw + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * cw),
            (a + 1.0) - (a - 1.0) * cw - sa,
        ),
        _ => return 0.0,
    };
    // |H(e^jw)| with z^-1 = e^-jw
    let w = 2.0 * std::f64::consts::PI * f / fs;
    let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
    let num = ((b0 + b1 * c1 + b2 * c2).powi(2) + (b1 * s1 + b2 * s2).powi(2)).sqrt();
    let den = ((a0 + a1 * c1 + a2 * c2).powi(2) + (a1 * s1 + a2 * s2).powi(2)).sqrt();
    20.0 * (num / den).max(1e-9).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic factory tuning with the usual 10-band layout.
    fn stock() -> EqTable {
        let g = [0.0, 0.0, 0.5, 1.5, -0.5, -1.0, -0.5, 1.0, 0.5, 0.0];
        let f = [25, 50, 100, 220, 450, 1000, 2500, 5000, 10000, 21000];
        let t = [2, 0, 2, 2, 2, 2, 2, 2, 2, 1];
        EqTable {
            global_gain: -3.0,
            enable_mask: 0x3FF,
            bands: (0..10)
                .map(|i| Band {
                    index: i as u16,
                    filter: t[i],
                    freq: f[i],
                    gain: g[i],
                    q: 0.7,
                })
                .collect(),
        }
    }

    #[test]
    fn peaking_hits_its_gain_at_centre() {
        let t = EqTable {
            global_gain: 0.0,
            enable_mask: 1,
            bands: vec![Band {
                index: 0,
                filter: PEAKING,
                freq: 1000,
                gain: 3.0,
                q: 1.0,
            }],
        };
        assert!((response_db(&t, 1000.0) - 3.0).abs() < 0.01);
        assert!(response_db(&t, 100.0).abs() < 0.2);
    }

    #[test]
    fn every_preset_is_driver_safe_and_has_headroom() {
        for p in &PRESETS[1..] {
            let t = apply(&stock(), p);
            assert_eq!(t.bands.len(), 10, "{}", p.name);
            assert_eq!(t.bands[1].filter, HIGH_PASS, "{}: high-pass must stay", p.name);
            assert!(t.bands[1].freq >= 50, "{}", p.name);
            assert!(
                p.bands.iter().chain([&p.bass_shelf]).all(|&g| g <= 3.0),
                "{}: boost too large",
                p.name
            );
            // overall peak (global gain included) never above the stock tuning's
            let overall = |t: &EqTable| log_freqs(20.0, 20000.0, 400).map(|f| response_db(t, f)).fold(f64::MIN, f64::max);
            let (peak, limit) = (overall(&t), overall(&stock()));
            assert!(peak <= limit + 0.01, "{}: peaks {peak:.2} dB, stock {limit:.2}", p.name);
            assert!(t.global_gain <= stock().global_gain, "{}", p.name);
            // well below the driver's range the output must be strongly reduced
            assert!(response_db(&t, 25.0) < -6.0, "{}: too much deep bass", p.name);
        }
    }

    #[test]
    fn stock_preset_reproduces_stock_mids() {
        let s = stock();
        assert_eq!(apply(&s, find("stock").unwrap()), s);
        // other presets keep the stock centres and add their offsets on top
        let t = apply(&s, find("pop").unwrap());
        for i in 2..9 {
            assert_eq!(t.bands[i].freq, s.bands[i].freq);
            assert!((t.bands[i].gain - (s.bands[i].gain + PRESETS[8].bands[i - 2])).abs() < 1e-6);
        }
    }

    #[test]
    fn find_is_forgiving() {
        assert_eq!(find("bass boost").unwrap().name, "Bass Boost");
        assert_eq!(find("hiphop").unwrap().name, "Hip-Hop");
        assert!(find("nope").is_none());
    }
}
