//! `eq_cfg_hw.bin`: EQ / DRC presets written by JieLi's EQ tool.
//!
//! Layout:
//! - 5-byte file header (kept as-is)
//! - directory `[u16 0xFFFD][u16 len]`: `u8 count`, `count × (u16 group, u16 table_off, u16 group_off)`,
//!   then per group a table `u8 n, n × (u16 chunk id, u16 chunk_off)` (absolute offsets)
//! - groups `[u16 group][u16 len]` holding chunks `[u16 crc16(id..data)][u16 id][u16 len][data]`
//! - trailer `[u16 0xFFFA][u16 len]`: `u32 BE size` + zlib-compressed EQ tool metadata (mode names)
//!
//! EQ chunks: `f32 global_gain, u32 band_count, u32 enable_mask`, then bands of
//! `{u16 index, u16 filter_type, i32 freq, f32 gain_db, f32 q}`.

use super::{f32_at, json_f32, put_f32, put_u16, u16_at};
use crate::formats::cfgtool::{hex, unhex};
use crate::jlfw::crc16;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

const DIR_ID: u16 = 0xFFFD;
const META_ID: u16 = 0xFFFA;

#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    pub id: u16,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: u16,
    pub chunks: Vec<Chunk>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EqFile {
    pub header: Vec<u8>,
    pub groups: Vec<Group>,
    pub meta: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Band {
    pub index: u16,
    pub filter: u16,
    pub freq: i32,
    pub gain: f32,
    pub q: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EqTable {
    pub global_gain: f32,
    pub enable_mask: u32,
    pub bands: Vec<Band>,
}

pub const FILTER_NAMES: [&str; 5] = ["high_pass", "low_pass", "peaking", "high_shelf", "low_shelf"];

pub fn filter_name(t: u16) -> String {
    FILTER_NAMES
        .get(t as usize)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("type_{t}"))
}

fn filter_from_name(s: &str) -> Option<u16> {
    FILTER_NAMES
        .iter()
        .position(|n| *n == s)
        .map(|p| p as u16)
        .or_else(|| s.strip_prefix("type_")?.parse().ok())
}

impl EqTable {
    pub fn parse(d: &[u8]) -> Option<EqTable> {
        if d.len() < 12 || !(d.len() - 12).is_multiple_of(16) {
            return None;
        }
        let n = u32::from_le_bytes(d[4..8].try_into().unwrap()) as usize;
        if n != (d.len() - 12) / 16 {
            return None;
        }
        let bands = (0..n)
            .map(|i| {
                let o = 12 + i * 16;
                Band {
                    index: u16_at(d, o),
                    filter: u16_at(d, o + 2),
                    freq: i32::from_le_bytes(d[o + 4..o + 8].try_into().unwrap()),
                    gain: f32_at(d, o + 8),
                    q: f32_at(d, o + 12),
                }
            })
            .collect();
        Some(EqTable {
            global_gain: f32_at(d, 0),
            enable_mask: u32::from_le_bytes(d[8..12].try_into().unwrap()),
            bands,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut d = vec![0u8; 12 + 16 * self.bands.len()];
        put_f32(&mut d, 0, self.global_gain);
        d[4..8].copy_from_slice(&(self.bands.len() as u32).to_le_bytes());
        d[8..12].copy_from_slice(&self.enable_mask.to_le_bytes());
        for (i, b) in self.bands.iter().enumerate() {
            let o = 12 + i * 16;
            put_u16(&mut d, o, b.index);
            put_u16(&mut d, o + 2, b.filter);
            d[o + 4..o + 8].copy_from_slice(&b.freq.to_le_bytes());
            put_f32(&mut d, o + 8, b.gain);
            put_f32(&mut d, o + 12, b.q);
        }
        d
    }
}

pub fn chunk_label(id: u16) -> &'static str {
    match id {
        0x2001 => "Music EQ",
        0x2002 => "Music DRC / limiter",
        0x1101..=0x1104 => "Call EQ",
        0x1105 | 0x1106 => "Call DRC / limiter",
        _ => "unknown",
    }
}

pub fn looks_like(d: &[u8]) -> bool {
    d.len() > 9 && u16_at(d, 5) == DIR_ID && parse(d).is_ok()
}

pub fn parse(d: &[u8]) -> Result<EqFile> {
    ensure!(d.len() > 9 && u16_at(d, 5) == DIR_ID, "no EQ directory at offset 5");
    let header = d[..5].to_vec();
    let dlen = u16_at(d, 7) as usize;
    let dir = d.get(9..9 + dlen).context("directory runs past the end")?;
    let count = dir[0] as usize;
    let mut pos = 9 + dlen;
    let mut groups = Vec::new();
    for g in 0..count {
        let e = 1 + g * 6;
        let (gid, goff) = (u16_at(dir, e), u16_at(dir, e + 4) as usize);
        ensure!(goff == pos, "group {gid:#x} expected at 0x{goff:X}, found at 0x{pos:X}");
        ensure!(u16_at(d, pos) == gid, "group id mismatch at 0x{pos:X}");
        let end = pos + 4 + u16_at(d, pos + 2) as usize;
        ensure!(end <= d.len(), "group {gid:#x} runs past the end");
        pos += 4;
        let mut chunks = Vec::new();
        while pos < end {
            let (crc, id, len) = (u16_at(d, pos), u16_at(d, pos + 2), u16_at(d, pos + 4) as usize);
            let body = d
                .get(pos + 2..pos + 6 + len)
                .with_context(|| format!("chunk {id:#x} runs past the end"))?;
            ensure!(crc16(body, 0) == crc, "chunk {id:#x} at 0x{pos:X}: CRC mismatch");
            chunks.push(Chunk {
                id,
                data: body[4..].to_vec(),
            });
            pos += 6 + len;
        }
        ensure!(pos == end, "group {gid:#x} length mismatch");
        groups.push(Group { id: gid, chunks });
    }
    ensure!(u16_at(d, pos) == META_ID, "expected metadata trailer at 0x{pos:X}");
    let mlen = u16_at(d, pos + 2) as usize;
    let meta = d.get(pos + 4..pos + 4 + mlen).context("metadata runs past the end")?.to_vec();
    ensure!(pos + 4 + mlen == d.len(), "trailing bytes after metadata");
    Ok(EqFile { header, groups, meta })
}

pub fn encode(f: &EqFile) -> Vec<u8> {
    let g = f.groups.len();
    let dir_len = 1 + 6 * g + f.groups.iter().map(|gr| 1 + 4 * gr.chunks.len()).sum::<usize>();
    let mut dir = vec![g as u8];
    let mut table_off = 9 + 1 + 6 * g;
    let mut group_off = 9 + dir_len;
    let mut tables = Vec::new();
    for gr in &f.groups {
        dir.extend_from_slice(&gr.id.to_le_bytes());
        dir.extend_from_slice(&(table_off as u16).to_le_bytes());
        dir.extend_from_slice(&(group_off as u16).to_le_bytes());
        tables.push(gr.chunks.len() as u8);
        let mut co = group_off + 4;
        for c in &gr.chunks {
            tables.extend_from_slice(&c.id.to_le_bytes());
            tables.extend_from_slice(&(co as u16).to_le_bytes());
            co += 6 + c.data.len();
        }
        table_off += 1 + 4 * gr.chunks.len();
        group_off = co;
    }
    dir.extend_from_slice(&tables);

    let mut out = f.header.clone();
    out.extend_from_slice(&DIR_ID.to_le_bytes());
    out.extend_from_slice(&(dir.len() as u16).to_le_bytes());
    out.extend_from_slice(&dir);
    for gr in &f.groups {
        let glen: usize = gr.chunks.iter().map(|c| 6 + c.data.len()).sum();
        out.extend_from_slice(&gr.id.to_le_bytes());
        out.extend_from_slice(&(glen as u16).to_le_bytes());
        for c in &gr.chunks {
            let mut body = c.id.to_le_bytes().to_vec();
            body.extend_from_slice(&(c.data.len() as u16).to_le_bytes());
            body.extend_from_slice(&c.data);
            out.extend_from_slice(&crc16(&body, 0).to_le_bytes());
            out.extend_from_slice(&body);
        }
    }
    out.extend_from_slice(&META_ID.to_le_bytes());
    out.extend_from_slice(&(f.meta.len() as u16).to_le_bytes());
    out.extend_from_slice(&f.meta);
    out
}

/// Mode names stored by the EQ tool (in group order), translated where known.
pub fn mode_names(f: &EqFile) -> Vec<String> {
    let Some(raw) = f.meta.get(4..).and_then(|z| miniz_oxide::inflate::decompress_to_vec_zlib(z).ok()) else {
        return Vec::new();
    };
    // first record: [u16 0xFFFF][u16 len] then length-prefixed UTF-8 names
    let mut names = Vec::new();
    let mut p = 4;
    while names.len() < f.groups.len() && p < raw.len() {
        let n = raw[p] as usize;
        let Some(s) = raw.get(p + 1..p + 1 + n).and_then(|b| std::str::from_utf8(b).ok()) else {
            break;
        };
        names.push(translate(s));
        p += 1 + n;
    }
    names
}

fn translate(s: &str) -> String {
    let en = match s {
        "音乐音效" => "Music",
        "通话下行音效" => "Call downlink (what you hear)",
        "通话上行音效" => "Call uplink (microphone)",
        _ => return s.to_string(),
    };
    en.to_string() // English only: the GUI font has no CJK glyphs
}

pub fn to_json(f: &EqFile) -> Value {
    let names = mode_names(f);
    let groups: Vec<Value> = f
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let chunks: Vec<Value> = g
                .chunks
                .iter()
                .map(|c| match EqTable::parse(&c.data) {
                    Some(t) => json!({
                        "id": format!("0x{:04X}", c.id),
                        "label": chunk_label(c.id),
                        "eq": {
                            "global_gain": json_f32(t.global_gain),
                            "enable_mask": format!("0x{:X}", t.enable_mask),
                            "bands": t.bands.iter().map(|b| json!({
                                "index": b.index,
                                "type": filter_name(b.filter),
                                "freq_hz": b.freq,
                                "gain_db": json_f32(b.gain),
                                "q": json_f32(b.q),
                            })).collect::<Vec<_>>(),
                        }
                    }),
                    None => json!({ "id": format!("0x{:04X}", c.id), "label": chunk_label(c.id), "hex": hex(&c.data) }),
                })
                .collect();
            json!({ "group": g.id, "name": names.get(i).cloned().unwrap_or_default(), "chunks": chunks })
        })
        .collect();
    json!({ "format": "jl-eq-cfg", "header": hex(&f.header), "groups": groups, "meta": hex(&f.meta) })
}

fn hex_u(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| u64::from_str_radix(v.as_str()?.trim_start_matches("0x"), 16).ok())
}

pub fn from_json(v: &Value) -> Result<EqFile> {
    if v["format"] != "jl-eq-cfg" {
        bail!("not a jl-eq-cfg document");
    }
    let header = unhex(v["header"].as_str().context("missing \"header\"")?)?;
    let meta = unhex(v["meta"].as_str().context("missing \"meta\"")?)?;
    let mut groups = Vec::new();
    for g in v["groups"].as_array().context("missing \"groups\"")? {
        let gid = g["group"].as_u64().context("bad \"group\"")? as u16;
        let mut chunks = Vec::new();
        for c in g["chunks"].as_array().context("missing \"chunks\"")? {
            let id = hex_u(&c["id"]).context("bad chunk \"id\"")? as u16;
            let data = if let Some(eq) = c.get("eq") {
                let f32_of = |x: &Value, what: &str| x.as_f64().map(|n| n as f32).with_context(|| format!("chunk {id:#x}: bad {what}"));
                let bands = eq["bands"]
                    .as_array()
                    .context("missing \"bands\"")?
                    .iter()
                    .map(|b| {
                        Ok(Band {
                            index: b["index"].as_u64().context("bad band index")? as u16,
                            filter: filter_from_name(b["type"].as_str().context("bad band type")?).context("unknown band type")?,
                            freq: b["freq_hz"].as_i64().context("bad freq_hz")? as i32,
                            gain: f32_of(&b["gain_db"], "gain_db")?,
                            q: f32_of(&b["q"], "q")?,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                EqTable {
                    global_gain: f32_of(&eq["global_gain"], "global_gain")?,
                    enable_mask: hex_u(&eq["enable_mask"]).context("bad enable_mask")? as u32,
                    bands,
                }
                .encode()
            } else {
                unhex(
                    c["hex"]
                        .as_str()
                        .with_context(|| format!("chunk {id:#x}: needs \"eq\" or \"hex\""))?,
                )?
            };
            chunks.push(Chunk { id, data });
        }
        groups.push(Group { id: gid, chunks });
    }
    Ok(EqFile { header, groups, meta })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EqFile {
        let t = EqTable {
            global_gain: 0.0,
            enable_mask: 0x3,
            bands: vec![
                Band {
                    index: 0,
                    filter: 0,
                    freq: 52,
                    gain: 0.33,
                    q: 0.7,
                },
                Band {
                    index: 1,
                    filter: 2,
                    freq: 1000,
                    gain: -3.5,
                    q: 1.4,
                },
            ],
        };
        EqFile {
            header: vec![2, 1, 0, 0, 0],
            groups: vec![
                Group {
                    id: 4,
                    chunks: vec![
                        Chunk {
                            id: 0x2002,
                            data: vec![7; 72],
                        },
                        Chunk {
                            id: 0x2001,
                            data: t.encode(),
                        },
                    ],
                },
                Group {
                    id: 1,
                    chunks: vec![Chunk {
                        id: 0x1101,
                        data: t.encode(),
                    }],
                },
            ],
            meta: vec![0, 0, 0, 0],
        }
    }

    #[test]
    fn binary_and_json_roundtrip() {
        let f = sample();
        let b = encode(&f);
        assert_eq!(parse(&b).unwrap(), f);
        assert_eq!(from_json(&to_json(&f)).unwrap(), f);
    }

    #[test]
    fn edit_band_via_json() {
        let mut j = to_json(&sample());
        j["groups"][0]["chunks"][1]["eq"]["bands"][1]["gain_db"] = json!(6.0);
        let f = from_json(&j).unwrap();
        let t = EqTable::parse(&f.groups[0].chunks[1].data).unwrap();
        assert_eq!(t.bands[1].gain, 6.0);
        assert!(parse(&encode(&f)).is_ok());
    }

    #[test]
    fn real_file_roundtrip() {
        let Some(data) = super::super::test_file("eq_cfg_hw.bin") else {
            return;
        };
        let f = parse(&data).unwrap();
        assert_eq!(encode(&f), data);
        assert_eq!(encode(&from_json(&to_json(&f)).unwrap()), data);
        assert_eq!(mode_names(&f).len(), f.groups.len());
    }
}
