//! `isd_config.ini` (binary, in the boot area): the encoded chip key followed by
//! download/boot settings as `[u8 value_len][key\0][value]` records. Read-only.

use super::text_at;
use crate::formats::cfgtool::hex;
use crate::jlfw::crc16;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct IsdConfig {
    /// Chip key decoded from the 32-byte blob, if its CRC is valid.
    pub chip_key: Option<u16>,
    pub entries: Vec<(String, Vec<u8>)>,
}

pub fn looks_like(d: &[u8]) -> bool {
    parse(d).is_ok_and(|c| c.chip_key.is_some() && !c.entries.is_empty())
}

/// Decode jl-misctools' "chipkey.bin" encoding of the key.
fn chipkey_decode(b: &[u8]) -> u16 {
    let mut sum = b[..16].iter().map(|&x| x as u32).sum::<u32>() as u8;
    if sum >= 0xE0 {
        sum = 0xAA;
    } else if sum <= 0x10 {
        sum = 0x55;
    }
    (0..16).filter(|&i| b[16 + i] ^ b[15 - i] < sum).fold(0, |k, i| k | 1 << i)
}

pub fn parse(d: &[u8]) -> Result<IsdConfig> {
    ensure!(d.len() >= 34, "too short for isd_config");
    let crc = u16::from_le_bytes([d[32], d[33]]);
    let chip_key = (crc16(&d[..32], 0) == crc).then(|| chipkey_decode(&d[..32]));
    let mut entries = Vec::new();
    let mut p = 34;
    while p < d.len() {
        let vlen = d[p] as usize;
        let key_end = d[p + 1..]
            .iter()
            .position(|&c| c == 0)
            .map(|e| p + 1 + e)
            .context("unterminated key")?;
        let key = String::from_utf8_lossy(&d[p + 1..key_end]).into_owned();
        let val = d
            .get(key_end + 1..key_end + 1 + vlen)
            .with_context(|| format!("value of {key} runs past the end"))?;
        entries.push((key, val.to_vec()));
        p = key_end + 1 + vlen;
    }
    Ok(IsdConfig { chip_key, entries })
}

/// Human-readable value: text when printable, else a little-endian number or hex.
pub fn show_value(v: &[u8]) -> String {
    if !v.is_empty() && v.iter().all(|&c| (0x20..0x7F).contains(&c)) {
        text_at(v)
    } else if v.len() == 4 {
        u32::from_le_bytes(v.try_into().unwrap()).to_string()
    } else {
        hex(v)
    }
}

pub fn to_json(c: &IsdConfig) -> Value {
    let entries: Map<String, Value> = c.entries.iter().map(|(k, v)| (k.clone(), json!(show_value(v)))).collect();
    json!({
        "format": "jl-isd-config",
        "note": "read-only: boot settings are not recompiled",
        "chip_key": c.chip_key.map(|k| format!("0x{k:04X}")),
        "entries": entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        let mut d = vec![0x11u8; 32];
        let c = crc16(&d, 0);
        d.extend_from_slice(&c.to_le_bytes());
        d.extend_from_slice(b"\x07SPI\x002_3_0_0\x04UTRX\x00PB05");
        let cfg = parse(&d).unwrap();
        assert!(cfg.chip_key.is_some());
        assert_eq!(
            cfg.entries,
            vec![("SPI".into(), b"2_3_0_0".to_vec()), ("UTRX".into(), b"PB05".to_vec())]
        );
    }

    #[test]
    fn real_file_key_matches_device() {
        let Some(data) = super::super::test_file("isd_config.ini") else {
            return;
        };
        let cfg = parse(&data).unwrap();
        let img_key = super::super::test_image().unwrap().chipkey;
        assert_eq!(cfg.chip_key, Some(img_key));
        assert!(cfg.entries.iter().any(|(k, _)| k == "UPDATE_JUMP"));
    }
}
