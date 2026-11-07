//! `cfg_tool.bin`: the speaker's settings (Bluetooth name, volumes, keys, prompts...).
//!
//! Layout: `u32 length-of-rest`, then items until the end:
//! `[u8 crc16(data) & 0xFF] [u8 id & 0xFF] [u16 LE: len << 4 | id >> 8] [data; len]`
//! IDs are the SDK's `CFG_*` numbers from `syscfg_id.h`; field layouts follow `user_cfg.h`.

use super::{f32_at, json_f32, put_f32, put_u16, text_at, u16_at};
use crate::jlfw::crc16;
use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

#[derive(Clone, Debug, PartialEq)]
pub struct CfgItem {
    pub id: u16,
    pub data: Vec<u8>,
    /// Whether the stored checksum matched when parsed.
    pub checksum_ok: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CfgTool {
    pub items: Vec<CfgItem>,
}

pub fn looks_like(data: &[u8]) -> bool {
    data.len() >= 8 && u32::from_le_bytes(data[..4].try_into().unwrap()) as usize + 4 == data.len() && parse(data).is_ok()
}

pub fn parse(data: &[u8]) -> Result<CfgTool> {
    if data.len() < 4 {
        bail!("too short for cfg_tool.bin");
    }
    let total = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    if total + 4 != data.len() {
        bail!("length field says {} bytes, file has {}", total + 4, data.len());
    }
    let mut items = Vec::new();
    let mut p = 4;
    while p < data.len() {
        if p + 4 > data.len() {
            bail!("truncated item header at 0x{p:X}");
        }
        let b = u16_at(data, p + 2);
        let id = data[p + 1] as u16 | (b & 0xF) << 8;
        let len = (b >> 4) as usize;
        let d = data
            .get(p + 4..p + 4 + len)
            .with_context(|| format!("item {id} at 0x{p:X} runs past the end"))?;
        items.push(CfgItem {
            id,
            data: d.to_vec(),
            checksum_ok: crc16(d, 0) as u8 == data[p],
        });
        p += 4 + len;
    }
    Ok(CfgTool { items })
}

pub fn encode(cfg: &CfgTool) -> Vec<u8> {
    let mut out = vec![0u8; 4];
    for it in &cfg.items {
        assert!(it.data.len() < 4096 && it.id < 4096, "item too large");
        out.push(crc16(&it.data, 0) as u8);
        out.push(it.id as u8);
        out.extend_from_slice(&((it.data.len() as u16) << 4 | it.id >> 8).to_le_bytes());
        out.extend_from_slice(&it.data);
    }
    let total = (out.len() - 4) as u32;
    out[..4].copy_from_slice(&total.to_le_bytes());
    out
}

pub fn item_name(id: u16) -> &'static str {
    match id {
        101 => "CFG_BT_NAME",
        102 => "CFG_BT_MAC_ADDR",
        103 => "BLE name (inferred)",
        104 => "BLE MAC address (inferred)",
        105 => "unknown (105)",
        524 => "CFG_AUDIO_ID",
        535 => "CFG_CHARGE_ID",
        536 => "CFG_LOWPOWER_V_ID",
        537 => "CFG_MIC_TYPE_ID",
        538 => "CFG_COMBINE_SYS_VOL_ID",
        539 => "CFG_COMBINE_CALL_VOL_ID",
        601 => "CFG_BT_RF_POWER_ID",
        602 => "CFG_TWS_PAIR_CODE_ID",
        603 => "CFG_AUTO_OFF_TIME_ID",
        604 => "CFG_AEC_ID",
        605 => "CFG_UI_TONE_STATUS_ID",
        606 => "CFG_KEY_MSG_ID",
        607 => "CFG_LRC_ID",
        615 => "BLE RF power (inferred)",
        1022 => "tool data (1022)",
        1023 => "SDK version string",
        _ => "unknown",
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    U8,
    U16,
    U16Hex,
    F32,
    Text(usize),
    Mac,
}

impl Kind {
    pub fn size(self) -> usize {
        match self {
            Kind::U8 => 1,
            Kind::U16 | Kind::U16Hex => 2,
            Kind::F32 => 4,
            Kind::Text(n) => n,
            Kind::Mac => 6,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub offset: usize,
    pub kind: Kind,
}

fn f(key: impl Into<String>, label: impl Into<String>, offset: usize, kind: Kind) -> Field {
    Field {
        key: key.into(),
        label: label.into(),
        offset,
        kind,
    }
}

const STATUS_NAMES: [&str; 14] = [
    "charge_start",
    "charge_full",
    "power_on",
    "power_off",
    "low_power",
    "max_volume",
    "phone_in",
    "phone_out",
    "phone_active",
    "bt_init_ok",
    "bt_connected",
    "bt_disconnected",
    "tws_connected",
    "tws_disconnected",
];
const KEY_EVENTS: [&str; 6] = ["short", "long", "hold", "up", "double", "triple"];

/// Field layout for an item, or None if the item is not understood (shown as hex).
pub fn schema(id: u16, len: usize) -> Option<Vec<Field>> {
    let fields = match id {
        101 if len.is_multiple_of(33) => (0..len / 33)
            .flat_map(|i| {
                [
                    f(
                        format!("name{}_enabled", i + 1),
                        format!("Name {} enabled", i + 1),
                        i * 33,
                        Kind::U8,
                    ),
                    f(format!("name{}", i + 1), format!("Name {}", i + 1), i * 33 + 1, Kind::Text(32)),
                ]
            })
            .collect(),
        102 if len == 6 => vec![f("mac", "Bluetooth MAC (FF = random)", 0, Kind::Mac)],
        103 if len == 32 => vec![f("ble_name", "BLE name", 0, Kind::Text(32))],
        104 if len == 6 => vec![f("ble_mac", "BLE MAC (FF = random)", 0, Kind::Mac)],
        524 if len == 4 => vec![
            f("enabled", "Enabled", 0, Kind::U8),
            f("max_volume", "Maximum system volume", 1, Kind::U8),
            f("power_on_volume", "Power-on (default) volume", 2, Kind::U8),
            f("prompt_volume", "Voice prompt volume", 3, Kind::U8),
        ],
        535 if len == 5 => vec![
            f("enabled", "Enabled", 0, Kind::U8),
            f("charge_at_power_on", "Allow charging while on", 1, Kind::U8),
            f("full_voltage_sel", "Full-charge voltage (setting index)", 2, Kind::U8),
            f("full_current_sel", "Full-charge current (setting index)", 3, Kind::U8),
            f("charge_current_sel", "Charge current (setting index)", 4, Kind::U8),
        ],
        536 if len == 4 => vec![
            f("low_battery_warning_10mv", "Low-battery warning (×10 mV)", 0, Kind::U16),
            f("power_off_voltage_10mv", "Power-off voltage (×10 mV)", 2, Kind::U16),
        ],
        537 if len == 3 => vec![
            f("mic_type", "Mic type (0 = cap-less off, 1 = on)", 0, Kind::U8),
            f("mic_pull_up", "Mic pull-up (setting index)", 1, Kind::U8),
            f("mic_ldo_level", "Mic LDO level (setting index)", 2, Kind::U8),
        ],
        601 | 615 if len == 1 => vec![f("rf_power", "RF transmit power", 0, Kind::U8)],
        602 if len == 2 => vec![f("tws_pair_code", "TWS pair code", 0, Kind::U16Hex)],
        603 if len == 1 => vec![f("auto_off_time", "Auto power-off time", 0, Kind::U8)],
        // AEC: a few byte-sized settings, then float parameters
        604 if len >= 4 && len.is_multiple_of(4) => (0..4)
            .map(|i| f(format!("aec_byte{i}"), format!("AEC setting byte {i}"), i, Kind::U8))
            .chain((1..len / 4).map(|i| f(format!("aec_{i}"), format!("AEC parameter {i}"), i * 4, Kind::F32)))
            .collect(),
        605 if len == 29 => std::iter::once(f("enabled", "Enabled", 0, Kind::U8))
            .chain(
                STATUS_NAMES
                    .iter()
                    .enumerate()
                    .map(|(i, n)| f(format!("led_{n}"), format!("LED pattern: {n}"), 1 + i, Kind::U8)),
            )
            .chain(
                STATUS_NAMES
                    .iter()
                    .enumerate()
                    .map(|(i, n)| f(format!("tone_{n}"), format!("Tone: {n}"), 15 + i, Kind::U8)),
            )
            .collect(),
        606 if len.is_multiple_of(6) => (0..len / 6)
            .flat_map(|k| {
                KEY_EVENTS
                    .iter()
                    .enumerate()
                    .map(move |(e, n)| f(format!("key{k}_{n}"), format!("Key {k} {n} press → message"), k * 6 + e, Kind::U8))
            })
            .collect(),
        607 if len == 9 => vec![
            f("lrc_ws_inc", "LRC ws inc", 0, Kind::U16),
            f("lrc_ws_init", "LRC ws init", 2, Kind::U16),
            f("btosc_ws_inc", "BT osc ws inc", 4, Kind::U16),
            f("btosc_ws_init", "BT osc ws init", 6, Kind::U16),
            f("lrc_change_mode", "LRC change mode", 8, Kind::U8),
        ],
        1023 => vec![f("version", "Version", 0, Kind::Text(len))],
        _ => return None,
    };
    Some(fields)
}

fn covers_all(fields: &[Field], len: usize) -> bool {
    fields.iter().map(|f| f.kind.size()).sum::<usize>() == len
}

pub fn read_field(data: &[u8], fl: &Field) -> Value {
    let o = fl.offset;
    match fl.kind {
        Kind::U8 => json!(data[o]),
        Kind::U16 => json!(u16_at(data, o)),
        Kind::U16Hex => json!(format!("0x{:04X}", u16_at(data, o))),
        Kind::F32 => json_f32(f32_at(data, o)),
        Kind::Text(n) => json!(text_at(&data[o..o + n])),
        Kind::Mac => json!(data[o..o + 6].iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")),
    }
}

pub fn write_field(data: &mut [u8], fl: &Field, v: &Value) -> Result<()> {
    let o = fl.offset;
    let bad = || anyhow::anyhow!("bad value for \"{}\": {v}", fl.key);
    match fl.kind {
        Kind::U8 => data[o] = v.as_u64().filter(|&n| n <= 0xFF).ok_or_else(bad)? as u8,
        Kind::U16 => put_u16(data, o, v.as_u64().filter(|&n| n <= 0xFFFF).ok_or_else(bad)? as u16),
        Kind::U16Hex => {
            let s = v.as_str().ok_or_else(bad)?;
            put_u16(data, o, u16::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| bad())?)
        }
        Kind::F32 => put_f32(data, o, v.as_f64().ok_or_else(bad)? as f32),
        Kind::Text(n) => {
            let s = v.as_str().ok_or_else(bad)?.as_bytes();
            if s.len() > n {
                bail!("\"{}\" is longer than {n} bytes", fl.key);
            }
            data[o..o + n].fill(0);
            data[o..o + s.len()].copy_from_slice(s);
        }
        Kind::Mac => {
            let parts: Vec<u8> = v
                .as_str()
                .ok_or_else(bad)?
                .split(':')
                .map(|p| u8::from_str_radix(p, 16))
                .collect::<Result<_, _>>()
                .map_err(|_| bad())?;
            if parts.len() != 6 {
                return Err(bad());
            }
            data[o..o + 6].copy_from_slice(&parts);
        }
    }
    Ok(())
}

/// Decompile to an editable JSON document.
pub fn to_json(cfg: &CfgTool) -> Value {
    let items: Vec<Value> = cfg
        .items
        .iter()
        .map(|it| {
            let mut m = Map::new();
            m.insert("id".into(), json!(it.id));
            m.insert("name".into(), json!(item_name(it.id)));
            m.insert("len".into(), json!(it.data.len()));
            match schema(it.id, it.data.len()) {
                Some(fields) => {
                    if !covers_all(&fields, it.data.len()) {
                        m.insert("hex".into(), json!(hex(&it.data)));
                    }
                    let vals: Map<String, Value> = fields.iter().map(|fl| (fl.key.clone(), read_field(&it.data, fl))).collect();
                    m.insert("fields".into(), Value::Object(vals));
                }
                None => {
                    m.insert("hex".into(), json!(hex(&it.data)));
                }
            }
            Value::Object(m)
        })
        .collect();
    json!({ "format": "jl-cfg-tool", "items": items })
}

/// Compile from the JSON produced by [`to_json`] (fields override the hex base).
pub fn from_json(v: &Value) -> Result<CfgTool> {
    if v["format"] != "jl-cfg-tool" {
        bail!("not a jl-cfg-tool document");
    }
    let mut items = Vec::new();
    for (i, it) in v["items"].as_array().context("missing \"items\"")?.iter().enumerate() {
        let id = it["id"]
            .as_u64()
            .filter(|&n| n < 4096)
            .with_context(|| format!("item {i}: bad \"id\""))? as u16;
        let mut data = match it.get("hex") {
            Some(h) => unhex(h.as_str().context("\"hex\" must be a string")?).with_context(|| format!("item {id}: bad hex"))?,
            None => vec![0; it["len"].as_u64().with_context(|| format!("item {id}: needs \"len\" or \"hex\""))? as usize],
        };
        if let Some(fields) = it.get("fields").and_then(Value::as_object) {
            let schema = schema(id, data.len()).with_context(|| format!("item {id}: no field layout for {} bytes", data.len()))?;
            for (k, val) in fields {
                let fl = schema
                    .iter()
                    .find(|f| &f.key == k)
                    .with_context(|| format!("item {id}: unknown field \"{k}\""))?;
                write_field(&mut data, fl, val).with_context(|| format!("item {id}"))?;
            }
        }
        items.push(CfgItem {
            id,
            data,
            checksum_ok: true,
        });
    }
    Ok(CfgTool { items })
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex(s: &str) -> Result<Vec<u8>> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !s.len().is_multiple_of(2) {
        bail!("odd number of hex digits");
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(Into::into))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let c = CfgTool {
            items: vec![
                CfgItem {
                    id: 101,
                    data: [&[1u8][..], b"Test Speaker", &[0u8; 20], &[0u8; 33]].concat(),
                    checksum_ok: true,
                },
                CfgItem {
                    id: 524,
                    data: vec![1, 31, 25, 25],
                    checksum_ok: true,
                },
                CfgItem {
                    id: 603,
                    data: vec![3],
                    checksum_ok: true,
                },
                CfgItem {
                    id: 1022,
                    data: vec![0xFF, 0x3F],
                    checksum_ok: true,
                },
                CfgItem {
                    id: 1023,
                    data: b"soundbox-1.0.0".to_vec(),
                    checksum_ok: true,
                },
            ],
        };
        encode(&c)
    }

    #[test]
    fn header_packing() {
        let b = sample();
        // item 524 = 0x20C: id low byte 0x0C, then len 4 << 4 | 0x2
        let p = b.windows(4).position(|w| w[1] == 0x0C && w[2] == 0x42 && w[3] == 0x00).unwrap();
        assert_eq!(b[p], crc16(&[1, 31, 25, 25], 0) as u8);
    }

    #[test]
    fn binary_roundtrip() {
        let b = sample();
        let c = parse(&b).unwrap();
        assert!(c.items.iter().all(|i| i.checksum_ok));
        assert_eq!(encode(&c), b);
    }

    #[test]
    fn json_roundtrip_and_edit() {
        let b = sample();
        let mut j = to_json(&parse(&b).unwrap());
        assert_eq!(from_json(&j).map(|c| encode(&c)).unwrap(), b);
        j["items"][1]["fields"]["power_on_volume"] = json!(12);
        j["items"][0]["fields"]["name1"] = json!("Renamed");
        let c = from_json(&j).unwrap();
        assert_eq!(c.items[1].data, [1, 31, 12, 25]);
        assert_eq!(&c.items[0].data[1..8], b"Renamed");
        assert!(c.items[0].data[8..33].iter().all(|&x| x == 0));
    }

    #[test]
    fn real_file_roundtrip() {
        let Some(data) = super::super::test_file("cfg_tool.bin") else {
            return;
        };
        let c = parse(&data).unwrap();
        assert!(c.items.iter().all(|i| i.checksum_ok));
        assert_eq!(encode(&c), data);
        assert_eq!(encode(&from_json(&to_json(&c)).unwrap()), data);
    }
}
