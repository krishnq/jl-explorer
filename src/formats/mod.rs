//! Decoders ("decompilers") and encoders ("compilers") for the files inside the firmware.

pub mod cfgtool;
pub mod eqcfg;
pub mod eqpresets;
pub mod isdcfg;

use anyhow::{bail, Result};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    CfgTool,
    EqCfg,
    IsdConfig,
}

impl Format {
    pub fn detect(name: &str, data: &[u8]) -> Option<Format> {
        match name {
            "cfg_tool.bin" => return Some(Format::CfgTool),
            "eq_cfg_hw.bin" => return Some(Format::EqCfg),
            "isd_config.ini" => return Some(Format::IsdConfig),
            _ => {}
        }
        if cfgtool::looks_like(data) {
            Some(Format::CfgTool)
        } else if eqcfg::looks_like(data) {
            Some(Format::EqCfg)
        } else if isdcfg::looks_like(data) {
            Some(Format::IsdConfig)
        } else {
            None
        }
    }
}

/// Binary to editable JSON.
pub fn decompile(fmt: Format, data: &[u8]) -> Result<Value> {
    Ok(match fmt {
        Format::CfgTool => cfgtool::to_json(&cfgtool::parse(data)?),
        Format::EqCfg => eqcfg::to_json(&eqcfg::parse(data)?),
        Format::IsdConfig => isdcfg::to_json(&isdcfg::parse(data)?),
    })
}

/// JSON (as produced by [`decompile`]) back to binary.
pub fn compile(v: &Value) -> Result<Vec<u8>> {
    match v["format"].as_str() {
        Some("jl-cfg-tool") => Ok(cfgtool::encode(&cfgtool::from_json(v)?)),
        Some("jl-eq-cfg") => Ok(eqcfg::encode(&eqcfg::from_json(v)?)),
        Some("jl-isd-config") => bail!("isd_config is read-only (boot settings); it cannot be compiled"),
        _ => bail!("unknown \"format\" (expected jl-cfg-tool or jl-eq-cfg)"),
    }
}

pub(crate) fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

pub(crate) fn put_u16(d: &mut [u8], o: usize, v: u16) {
    d[o..o + 2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn f32_at(d: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

pub(crate) fn put_f32(d: &mut [u8], o: usize, v: f32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}

/// NUL-terminated text.
pub(crate) fn text_at(d: &[u8]) -> String {
    let end = d.iter().position(|&c| c == 0).unwrap_or(d.len());
    String::from_utf8_lossy(&d[..end]).into_owned()
}

/// f32 as a JSON number using its shortest decimal form (0.7, not 0.699999988).
pub(crate) fn json_f32(x: f32) -> Value {
    x.to_string().parse::<f64>().map(|f| json!(f)).unwrap_or(Value::Null)
}

#[cfg(test)]
pub(crate) fn test_image() -> Option<crate::jlfw::Image> {
    let raw = std::fs::read(std::env::var("JL_TEST_DUMP").ok()?).ok()?;
    crate::jlfw::Image::parse(raw, None).ok()
}

/// A file from the image in JL_TEST_DUMP, by name (app-area resources or boot-area entries).
#[cfg(test)]
pub(crate) fn test_file(name: &str) -> Option<Vec<u8>> {
    use crate::jlfw::ResKind;
    let img = test_image()?;
    if let Some(i) = img.top.iter().position(|t| t.hdr.name() == name) {
        return Some(img.top_data(i).to_vec());
    }
    img.res.iter().find(|r| r.name() == name).and_then(|r| match &r.kind {
        ResKind::File { data, .. } => Some(data.clone()),
        _ => None,
    })
}
