//! jlcfg: decompile firmware files to editable JSON and compile them back.
//!
//!   jlcfg decompile <file.bin> [out.json]   binary -> JSON (stdout if no output)
//!   jlcfg compile <file.json> <out.bin>     JSON -> binary
//!   jlcfg unpack <flash.bin> <dir>          extract every file of a flash image
//!   jlcfg repack <flash.bin> <dir> <out.bin> rebuild a flash image with files from <dir>
//!   jlcfg eq-presets                        list the built-in music EQ presets
//!   jlcfg eq-preset <eq.bin> <name> <out>   apply a preset on top of the stock tuning in <eq.bin>

use anyhow::{bail, Context, Result};
use jl_explorer::formats::{self, eqcfg, eqpresets, Format};
use jl_explorer::jlfw::{Image, ResKind};
use std::path::Path;

fn main() {
    if let Err(e) = run(std::env::args().skip(1).collect()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<()> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["decompile", input, rest @ ..] if rest.len() <= 1 => {
            let data = std::fs::read(input).with_context(|| format!("reading {input}"))?;
            let name = file_name(input);
            let fmt = Format::detect(&name, &data).context("unrecognised file (supported: cfg_tool.bin, eq_cfg_hw.bin, isd_config.ini)")?;
            let text = serde_json::to_string_pretty(&formats::decompile(fmt, &data)?)? + "\n";
            match rest.first() {
                Some(out) => std::fs::write(out, text).with_context(|| format!("writing {out}"))?,
                None => print!("{text}"),
            }
        }
        ["compile", input, out] => {
            let text = std::fs::read_to_string(input).with_context(|| format!("reading {input}"))?;
            let v: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {input}"))?;
            let bin = formats::compile(&v)?;
            std::fs::write(out, &bin).with_context(|| format!("writing {out}"))?;
            eprintln!("wrote {} bytes to {out}", bin.len());
        }
        ["eq-presets"] => {
            for p in eqpresets::PRESETS {
                println!("{:<24} {}", p.name, p.description);
            }
        }
        ["eq-preset", input, name, out] => {
            let preset = eqpresets::find(name).with_context(|| format!("unknown preset \"{name}\" (see: jlcfg eq-presets)"))?;
            let mut f = eqcfg::parse(&std::fs::read(input).with_context(|| format!("reading {input}"))?)?;
            let chunk = f.groups.iter_mut().flat_map(|g| &mut g.chunks).find(|c| c.id == 0x2001).context("no music EQ (0x2001) in this file")?;
            let stock = eqcfg::EqTable::parse(&chunk.data).context("music EQ is not a band table")?;
            let t = eqpresets::apply(&stock, preset);
            chunk.data = t.encode();
            std::fs::write(out, eqcfg::encode(&f)).with_context(|| format!("writing {out}"))?;
            eprintln!("\"{}\" applied (global gain {:+.1} dB) -> {out}", preset.name, t.global_gain);
        }
        ["unpack", flash, dir] => {
            let img = Image::parse(std::fs::read(flash)?, None)?;
            let dir = Path::new(dir);
            std::fs::create_dir_all(dir)?;
            for (i, t) in img.top.iter().enumerate() {
                if t.hdr.flags & 0x10 == 0 && t.hdr.name() != "app_dir_head" {
                    write(dir, &format!("boot/{}", t.hdr.name()), img.top_data(i))?;
                }
            }
            for (i, s) in img.app_subs.iter().enumerate() {
                if s.hdr.name() == "app.bin" {
                    write(dir, "app/app.bin", img.app_sub_data(i))?;
                }
            }
            for r in &img.res {
                match &r.kind {
                    ResKind::File { data, .. } => write(dir, &format!("app/{}", r.name()), data)?,
                    ResKind::Dir { children } => {
                        for c in children {
                            write(dir, &format!("app/{}/{}", r.name(), c.hdr.name()), &c.data)?;
                        }
                    }
                    ResKind::Raw { .. } => {}
                }
            }
            eprintln!("chip key 0x{:04X}; editable files are under {}/app", img.chipkey, dir.display());
        }
        ["repack", flash, dir, out] => {
            let original = std::fs::read(flash)?;
            let mut img = Image::parse(original.clone(), None)?;
            let dir = Path::new(dir);
            for r in &mut img.res {
                let name = r.name();
                match &mut r.kind {
                    ResKind::File { data, .. } => replace(dir, &format!("app/{name}"), data)?,
                    ResKind::Dir { children } => {
                        for c in children {
                            replace(dir, &format!("app/{name}/{}", c.hdr.name()), &mut c.data)?;
                        }
                    }
                    ResKind::Raw { .. } => {}
                }
            }
            let built = img.build()?.image;
            std::fs::write(out, &built)?;
            let changed = jl_explorer::jlfw::changed_sectors(&original, &built);
            eprintln!("wrote {out}; {} sector(s) differ from {flash}", changed.len());
        }
        _ => bail!(
            "usage:\n  jlcfg decompile <file.bin> [out.json]\n  jlcfg compile <file.json> <out.bin>\n  jlcfg unpack <flash.bin> <dir>\n  jlcfg repack <flash.bin> <dir> <out.bin>\n  jlcfg eq-presets\n  jlcfg eq-preset <stock eq_cfg_hw.bin> <preset> <out.bin>"
        ),
    }
    Ok(())
}

fn file_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn write(dir: &Path, rel: &str, data: &[u8]) -> Result<()> {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, data).with_context(|| format!("writing {}", p.display()))
}

/// Replace `data` with the file at `dir/rel` if it exists and differs.
fn replace(dir: &Path, rel: &str, data: &mut Vec<u8>) -> Result<()> {
    let p = dir.join(rel);
    if p.exists() {
        let new = std::fs::read(&p)?;
        if &new != data {
            eprintln!("  replacing {rel} ({} -> {} bytes)", data.len(), new.len());
            *data = new;
        }
    }
    Ok(())
}
