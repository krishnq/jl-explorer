//! JieLi "new firmware" (BR23+) flash image format: parsing, editing and rebuilding.
//!
//! Layout of a BR25 flash image:
//! - flash header (32 bytes, ENC-scrambled with key 0xFFFF) at 0x1000 on most devices
//! - top-level entries (uboot.boot, isd_config.ini, app_dir_head, reserved areas)
//! - the app area, SFC-encrypted with the chip key: a chain of entries where the
//!   first one (app_area_head) holds app.bin and the reserved areas, followed by
//!   resource entries (plain files, or directories such as "tone")

use anyhow::{bail, Context, Result};

pub const SECTOR: usize = 0x1000;

const FLAG_RESERVED: u8 = 0x10;
const FLAG_FILE: u8 = 0x82;
const FLAG_DIR: u8 = 0x83;

/// CRC-16/XMODEM, as used throughout JieLi firmware.
pub fn crc16(data: &[u8], init: u16) -> u16 {
    let mut crc = init;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// JieLi "ENC" cipher (symmetric).
pub fn enc_cipher(buf: &mut [u8], mut key: u16) {
    for b in buf {
        *b ^= key as u8;
        key = (key << 1) ^ if key & 0x8000 != 0 { 0x1021 } else { 0 };
    }
}

/// SFC cipher: ENC in 32-byte blocks keyed by position. `rel` is the offset of
/// `buf[0]` from the start of the encrypted area and must be 32-byte aligned.
pub fn sfc_cipher(buf: &mut [u8], rel: usize, key: u16) {
    for (i, chunk) in buf.chunks_mut(32).enumerate() {
        enc_cipher(chunk, key ^ ((rel + i * 32) >> 2) as u16);
    }
}

/// JieLi "CrcDecode" (MengLi) cipher, used to obfuscate the chip key in loader responses.
pub fn mengli_cipher(buf: &mut [u8], key: u32) {
    const MAGIC: [u8; 16] = [
        0xC3, 0xCF, 0xC0, 0xE8, 0xCE, 0xD2, 0xB0, 0xAE, 0xC4, 0xE3, 0xA3, 0xAC, 0xD3, 0xF1, 0xC1, 0xD6,
    ];
    let mut crc = crc16(&((key >> 16) as u16).to_le_bytes(), key as u16);
    for (i, b) in buf.iter_mut().enumerate() {
        crc = crc16(&MAGIC[i % 16..i % 16 + 1], crc);
        *b ^= crc as u8;
    }
}

fn align(x: usize, a: usize) -> usize {
    x.div_ceil(a) * a
}

/// A 32-byte file system entry header.
#[derive(Clone, Debug, PartialEq)]
pub struct Hdr {
    pub dcrc: u16,
    pub offset: u32,
    pub size: u32,
    pub flags: u8,
    pub resvd: u8,
    pub index: u16,
    pub raw_name: [u8; 16],
}

impl Hdr {
    pub fn parse(b: &[u8]) -> Result<Hdr> {
        if b.len() < 32 {
            bail!("truncated entry header");
        }
        let hcrc = u16::from_le_bytes([b[0], b[1]]);
        if crc16(&b[2..32], 0) != hcrc {
            bail!("entry header CRC mismatch");
        }
        let u32le = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Ok(Hdr {
            dcrc: u16::from_le_bytes([b[2], b[3]]),
            offset: u32le(4),
            size: u32le(8),
            flags: b[12],
            resvd: b[13],
            index: u16::from_le_bytes([b[14], b[15]]),
            raw_name: b[16..32].try_into().unwrap(),
        })
    }

    pub fn write(&self, out: &mut [u8]) {
        out[2..4].copy_from_slice(&self.dcrc.to_le_bytes());
        out[4..8].copy_from_slice(&self.offset.to_le_bytes());
        out[8..12].copy_from_slice(&self.size.to_le_bytes());
        out[12] = self.flags;
        out[13] = self.resvd;
        out[14..16].copy_from_slice(&self.index.to_le_bytes());
        out[16..32].copy_from_slice(&self.raw_name);
        let c = crc16(&out[2..32], 0);
        out[0..2].copy_from_slice(&c.to_le_bytes());
    }

    pub fn name(&self) -> String {
        let end = self.raw_name.iter().position(|&c| c == 0).unwrap_or(16);
        String::from_utf8_lossy(&self.raw_name[..end]).into_owned()
    }

    fn is_last(&self) -> bool {
        self.index != 0
    }
}

#[derive(Clone, Debug)]
pub struct FlashHeader {
    pub offset: usize,
    pub vid: String,
    pub pid: String,
    pub flash_size: u32,
}

/// Top-level entry (outside the encrypted app area). Read-only.
#[derive(Clone, Debug)]
pub struct TopEntry {
    pub hdr: Hdr,
    pub data_abs: usize,
}

/// Sub-entry of app_area_head (app.bin and reserved areas like VM). Read-only.
#[derive(Clone, Debug)]
pub struct AppSub {
    pub hdr: Hdr,
    pub abs: usize,
}

#[derive(Clone, Debug)]
pub struct Child {
    pub hdr: Hdr,
    pub orig: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum ResKind {
    File {
        orig: Vec<u8>,
        data: Vec<u8>,
    },
    Dir {
        children: Vec<Child>,
    },
    /// Anything we don't understand, kept byte-for-byte (header included).
    Raw {
        bytes: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub struct Res {
    pub hdr: Hdr,
    pub orig_abs: usize,
    pub kind: ResKind,
}

impl Res {
    pub fn name(&self) -> String {
        self.hdr.name()
    }

    /// Serialize this entry (header included). Directories keep their original
    /// size when the new contents fit, so the rest of the flash does not move.
    fn serialize(&self) -> Vec<u8> {
        let mut hdr = self.hdr.clone();
        match &self.kind {
            ResKind::Raw { bytes } => bytes.clone(),
            ResKind::File { data, .. } => {
                let mut out = vec![0u8; 32 + data.len()];
                out[32..].copy_from_slice(data);
                hdr.size = out.len() as u32;
                hdr.dcrc = crc16(data, 0);
                hdr.write(&mut out[..32]);
                out
            }
            ResKind::Dir { children } => {
                let size = dir_required_size(children).max(self.hdr.size as usize);
                let mut out = vec![0xFFu8; size];
                let mut pos = 32 + 32 * children.len();
                for (i, c) in children.iter().enumerate() {
                    pos = align(pos, 16);
                    out[pos..pos + c.data.len()].copy_from_slice(&c.data);
                    let mut ch = c.hdr.clone();
                    ch.offset = pos as u32;
                    ch.size = c.data.len() as u32;
                    ch.dcrc = crc16(&c.data, 0);
                    ch.write(&mut out[32 + 32 * i..64 + 32 * i]);
                    pos += c.data.len();
                }
                hdr.size = size as u32;
                hdr.dcrc = crc16(&out[32..], 0);
                hdr.write(&mut out[..32]);
                out
            }
        }
    }

    pub fn is_modified(&self) -> bool {
        match &self.kind {
            ResKind::File { orig, data } => orig != data,
            ResKind::Dir { children } => children.iter().any(|c| c.orig != c.data),
            ResKind::Raw { .. } => false,
        }
    }
}

/// Bytes a directory needs for its headers and contents (tight packing).
pub fn dir_required_size(children: &[Child]) -> usize {
    let mut pos = 32 + 32 * children.len();
    for c in children {
        pos = align(pos, 16) + c.data.len();
    }
    pos
}

#[derive(Clone, Debug)]
pub struct Image {
    /// The image exactly as stored in flash (encrypted).
    pub raw: Vec<u8>,
    /// Decrypted copy of `raw` (top-level headers and app area).
    pub dec: Vec<u8>,
    pub chipkey: u16,
    pub header: FlashHeader,
    pub top: Vec<TopEntry>,
    pub appbase: usize,
    /// End of the app area entry chain.
    pub app_end: usize,
    /// The app area must not grow past this (start of the next reserved area).
    pub app_limit: usize,
    pub app_head: Hdr,
    pub app_subs: Vec<AppSub>,
    pub res: Vec<Res>,
}

pub struct Build {
    pub image: Vec<u8>,
}

impl Image {
    /// Parse a raw flash image. When `chipkey` is None (or wrong) the key is
    /// recovered by trying all 65536 keys against the app_area_head header.
    pub fn parse(raw: Vec<u8>, chipkey: Option<u16>) -> Result<Image> {
        let mut dec = raw.clone();

        let header = find_header(&raw)?;
        let base = header.offset;

        let mut top = Vec::new();
        let mut appbase = None;
        let mut off = base + 32;
        loop {
            if off + 32 > dec.len() {
                bail!("top-level directory runs past the end of the image");
            }
            enc_cipher(&mut dec[off..off + 32], 0xFFFF);
            let hdr = Hdr::parse(&dec[off..off + 32]).with_context(|| format!("top entry at 0x{off:X}"))?;
            let data_abs = base + hdr.offset as usize;
            if hdr.name() == "app_dir_head" && appbase.is_none() {
                appbase = Some(data_abs);
            }
            let last = hdr.is_last();
            top.push(TopEntry { hdr, data_abs });
            off += 32;
            if last {
                break;
            }
        }
        let appbase = appbase.context("no app_dir_head in the flash header")?;

        let chipkey = match chipkey.filter(|&k| check_key(&raw, appbase, k)) {
            Some(k) => k,
            None => find_chipkey(&raw, appbase).context("could not determine the chip key")?,
        };

        // Decrypt the app area entry by entry, like the boot ROM walks it.
        let mut ents: Vec<(usize, Hdr)> = Vec::new();
        let mut decd = appbase;
        let mut rel = 0usize;
        let app_end;
        loop {
            let ent = appbase + rel;
            let nxt = align(ent + 32, 32);
            if nxt > dec.len() {
                bail!("app area runs past the end of the image");
            }
            if decd < nxt {
                sfc_cipher(&mut dec[decd..nxt], decd - appbase, chipkey);
                decd = nxt;
            }
            let hdr = Hdr::parse(&dec[ent..ent + 32]).with_context(|| format!("app entry at 0x{ent:X}"))?;
            rel += hdr.size as usize;
            let last = hdr.is_last();
            let nxt = if last { appbase + rel } else { align(appbase + rel + 32, 32) };
            if nxt > dec.len() {
                bail!("app entry at 0x{ent:X} runs past the end of the image");
            }
            if decd < nxt {
                sfc_cipher(&mut dec[decd..nxt], decd - appbase, chipkey);
                decd = nxt;
            }
            ents.push((ent, hdr));
            if last {
                app_end = appbase + rel;
                break;
            }
        }

        let (head_off, app_head) = ents[0].clone();
        let mut app_subs = Vec::new();
        let mut sub = head_off + 32;
        while sub + 32 <= head_off + app_head.size as usize {
            let hdr = Hdr::parse(&dec[sub..sub + 32]).with_context(|| format!("app_area_head sub-entry at 0x{sub:X}"))?;
            let abs = head_off + hdr.offset as usize;
            let last = hdr.is_last();
            app_subs.push(AppSub { hdr, abs });
            sub += 32;
            if last {
                break;
            }
        }

        let mut res = Vec::new();
        for (ent, hdr) in ents.into_iter().skip(1) {
            let end = ent + hdr.size as usize;
            let kind = if hdr.flags == FLAG_FILE {
                let data = dec[ent + 32..end].to_vec();
                ResKind::File { orig: data.clone(), data }
            } else if hdr.flags == FLAG_DIR {
                match parse_dir(&dec[ent..end]) {
                    Ok(children) => ResKind::Dir { children },
                    Err(_) => ResKind::Raw {
                        bytes: dec[ent..end].to_vec(),
                    },
                }
            } else {
                ResKind::Raw {
                    bytes: dec[ent..end].to_vec(),
                }
            };
            res.push(Res { hdr, orig_abs: ent, kind });
        }

        // The app area may grow up to the next reserved region (e.g. VM, PRCT end).
        let mut app_limit = raw.len();
        let reserved = app_subs
            .iter()
            .map(|s| (s.hdr.flags, s.abs, s.hdr.size as usize))
            .chain(top.iter().map(|t| (t.hdr.flags, t.data_abs, t.hdr.size as usize)));
        for (flags, start, size) in reserved {
            if flags & FLAG_RESERVED == 0 {
                continue;
            }
            if start >= app_end {
                app_limit = app_limit.min(start);
            } else if start.saturating_add(size) >= app_end {
                app_limit = app_limit.min(start + size);
            }
        }

        Ok(Image {
            raw,
            dec,
            chipkey,
            header,
            top,
            appbase,
            app_end,
            app_limit,
            app_head,
            app_subs,
            res,
        })
    }

    pub fn is_modified(&self) -> bool {
        self.res.iter().any(Res::is_modified)
    }

    /// Size the app area would have after a rebuild.
    pub fn built_app_end(&self) -> usize {
        let mut pos = self.appbase + self.app_head.size as usize;
        for r in &self.res {
            pos += match &r.kind {
                ResKind::Raw { bytes } => bytes.len(),
                ResKind::File { data, .. } => 32 + data.len(),
                ResKind::Dir { children } => dir_required_size(children).max(r.hdr.size as usize),
            };
        }
        pos
    }

    /// Rebuild the full flash image with the current edits applied.
    pub fn build(&self) -> Result<Build> {
        let mut dec = self.dec.clone();
        let mut pos = self.appbase + self.app_head.size as usize;
        for r in &self.res {
            let blob = r.serialize();
            if pos + blob.len() > self.app_limit {
                bail!(
                    "\"{}\" does not fit: the app area would end at 0x{:X} but must stay below 0x{:X}",
                    r.name(),
                    pos + blob.len(),
                    self.app_limit
                );
            }
            dec[pos..pos + blob.len()].copy_from_slice(&blob);
            pos += blob.len();
        }
        let new_end = pos;

        let mut image = self.raw.clone();
        let mut region = dec[self.appbase..new_end].to_vec();
        sfc_cipher(&mut region, 0, self.chipkey);
        image[self.appbase..new_end].copy_from_slice(&region);
        if new_end < self.app_end {
            image[new_end..self.app_end].fill(0xFF);
        }
        Ok(Build { image })
    }

    /// Bytes stored in a top-level entry (as stored; uboot.boot stays scrambled).
    pub fn top_data(&self, i: usize) -> &[u8] {
        let t = &self.top[i];
        let start = t.data_abs.min(self.dec.len());
        let end = (t.data_abs + t.hdr.size as usize).min(self.dec.len());
        &self.dec[start..end]
    }

    /// Bytes of an app_area_head sub-entry (decrypted where it lies in the app area).
    pub fn app_sub_data(&self, i: usize) -> &[u8] {
        let s = &self.app_subs[i];
        let start = s.abs.min(self.dec.len());
        let end = (s.abs + s.hdr.size as usize).min(self.dec.len());
        if start < self.app_end {
            &self.dec[start..end.min(self.app_end)]
        } else {
            &self.raw[start..end]
        }
    }
}

fn parse_dir(blob: &[u8]) -> Result<Vec<Child>> {
    let mut children = Vec::new();
    let mut off = 32;
    loop {
        if off + 32 > blob.len() {
            bail!("directory entry list runs past the directory");
        }
        let hdr = Hdr::parse(&blob[off..off + 32])?;
        let (s, e) = (hdr.offset as usize, hdr.offset as usize + hdr.size as usize);
        if e > blob.len() {
            bail!("directory child out of bounds");
        }
        let data = blob[s..e].to_vec();
        if crc16(&data, 0) != hdr.dcrc {
            bail!("directory child data CRC mismatch");
        }
        let last = hdr.is_last();
        children.push(Child {
            hdr,
            orig: data.clone(),
            data,
        });
        off += 32;
        if last {
            return Ok(children);
        }
    }
}

fn find_header(raw: &[u8]) -> Result<FlashHeader> {
    for off in [0usize, 0x1000, 0x10000, 0x80000, 0x100000] {
        if off + 32 > raw.len() {
            continue;
        }
        let mut h = raw[off..off + 32].to_vec();
        enc_cipher(&mut h, 0xFFFF);
        let crc = u16::from_le_bytes([h[0], h[1]]);
        if crc != 0 && crc16(&h[2..], 0) == crc {
            // VID and PID strings are stored unscrambled
            let text = |b: &[u8]| String::from_utf8_lossy(b).trim_end_matches(['\0', '\u{FFFD}']).to_string();
            return Ok(FlashHeader {
                offset: off,
                vid: text(&raw[off + 4..off + 8]),
                pid: text(&raw[off + 16..off + 32]),
                flash_size: u32::from_le_bytes(h[8..12].try_into().unwrap()),
            });
        }
    }
    bail!("no JieLi flash header found (is this a BR23+ flash dump?)")
}

fn check_key(raw: &[u8], appbase: usize, key: u16) -> bool {
    if appbase + 32 > raw.len() {
        return false;
    }
    let mut h = raw[appbase..appbase + 32].to_vec();
    enc_cipher(&mut h, key);
    matches!(Hdr::parse(&h), Ok(hdr) if hdr.name() == "app_area_head")
}

pub fn find_chipkey(raw: &[u8], appbase: usize) -> Option<u16> {
    (0..=0xFFFFu16).find(|&k| check_key(raw, appbase, k))
}

/// 4K sectors that differ between two images of the same size.
pub fn changed_sectors(a: &[u8], b: &[u8]) -> Vec<usize> {
    (0..a.len().min(b.len()))
        .step_by(SECTOR)
        .filter(|&s| a[s..(s + SECTOR).min(a.len())] != b[s..(s + SECTOR).min(b.len())])
        .collect()
}

/// Rough content type for display.
pub fn guess_type(name: &str, data: &[u8]) -> &'static str {
    if data.starts_with(b"ID3") || (data.len() > 1 && data[0] == 0xFF && data[1] & 0xE0 == 0xE0) {
        "MP3 audio"
    } else if name.ends_with(".wtg") {
        "WTG tone sequence (synthesized beeps)"
    } else if data.starts_with(b"TIDX") {
        "Tone index"
    } else if name.ends_with(".ini")
        || (!data.is_empty()
            && data
                .iter()
                .all(|&c| c == b'\r' || c == b'\n' || c == b'\t' || (0x20..0x7F).contains(&c)))
    {
        "Text"
    } else {
        "Binary"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set JL_TEST_DUMP to a real BR25 flash dump to run the image tests.
    fn dump() -> Option<Vec<u8>> {
        let path = std::env::var("JL_TEST_DUMP").ok()?;
        Some(std::fs::read(path).expect("read JL_TEST_DUMP"))
    }

    #[test]
    fn crc_and_cipher_basics() {
        assert_eq!(crc16(b"123456789", 0), 0x31C3); // CRC-16/XMODEM check value
        let mut b = *b"hello world, this is a test....!!";
        let orig = b;
        sfc_cipher(&mut b, 64, 0xA5C3);
        assert_ne!(b, orig);
        sfc_cipher(&mut b, 64, 0xA5C3);
        assert_eq!(b, orig);
    }

    #[test]
    fn roundtrip_is_byte_identical() {
        let Some(raw) = dump() else { return };
        let img = Image::parse(raw.clone(), None).unwrap();
        assert!(!img.is_modified());
        assert_eq!(img.build().unwrap().image, raw);
    }

    #[test]
    fn chipkey_recovered_without_hint() {
        let Some(raw) = dump() else { return };
        let a = Image::parse(raw.clone(), None).unwrap();
        let b = Image::parse(raw, Some(a.chipkey ^ 0x1234)).unwrap();
        assert_eq!(a.chipkey, b.chipkey);
    }

    #[test]
    fn replace_child_in_place_and_reparse() {
        let Some(raw) = dump() else { return };
        let mut img = Image::parse(raw.clone(), None).unwrap();
        let (ri, ci) = first_child(&img);
        let new_data = vec![0x5Au8; 1000];
        set_child(&mut img, ri, ci, new_data.clone());
        let built = img.build().unwrap();
        let img2 = Image::parse(built.image.clone(), Some(img.chipkey)).unwrap();
        // directory keeps its size, so the app area end does not move
        assert_eq!(img2.app_end, img.app_end);
        let ResKind::Dir { children } = &img2.res[ri].kind else { panic!() };
        assert_eq!(children[ci].data, new_data);
        // everything outside the app area is untouched
        assert_eq!(built.image[..img.appbase], raw[..img.appbase]);
        assert_eq!(built.image[img.app_end..], raw[img.app_end..]);
    }

    #[test]
    fn growing_a_dir_shifts_following_entries() {
        let Some(raw) = dump() else { return };
        let mut img = Image::parse(raw, None).unwrap();
        let (ri, ci) = first_child(&img);
        let ResKind::Dir { children } = &img.res[ri].kind else { panic!() };
        let grow = img.app_limit - img.app_end - 64;
        let bigger = vec![0xA5u8; children[ci].data.len() + grow / 2];
        set_child(&mut img, ri, ci, bigger.clone());
        let built = img.build().unwrap();
        let img2 = Image::parse(built.image, Some(img.chipkey)).unwrap();
        assert!(img2.app_end > img.app_end);
        assert_eq!(img2.app_end, img.built_app_end());
        let ResKind::Dir { children } = &img2.res[ri].kind else { panic!() };
        assert_eq!(children[ci].data, bigger);
        for (a, b) in img.res.iter().zip(&img2.res).skip(ri + 1) {
            assert_eq!(a.serialize()[32..], b.serialize()[32..], "{} changed", a.name());
        }
    }

    /// JL_TEST_TONES: directory of replacement tone files; JL_TEST_EXPECTED: image known to work.
    #[test]
    fn rebuild_matches_known_good_image() {
        let (Some(raw), Ok(tones), Ok(expected)) = (dump(), std::env::var("JL_TEST_TONES"), std::env::var("JL_TEST_EXPECTED")) else {
            return;
        };
        let mut img = Image::parse(raw, None).unwrap();
        for r in &mut img.res {
            if let ResKind::Dir { children } = &mut r.kind {
                for c in children {
                    if let Ok(d) = std::fs::read(std::path::Path::new(&tones).join(c.hdr.name())) {
                        c.data = d;
                    }
                }
            }
        }
        assert!(img.build().unwrap().image == std::fs::read(expected).unwrap());
    }

    #[test]
    fn oversize_is_rejected() {
        let Some(raw) = dump() else { return };
        let mut img = Image::parse(raw, None).unwrap();
        let (ri, ci) = first_child(&img);
        let too_big = vec![0; img.app_limit];
        set_child(&mut img, ri, ci, too_big);
        assert!(img.build().is_err());
    }

    fn first_child(img: &Image) -> (usize, usize) {
        let ri = img
            .res
            .iter()
            .position(|r| matches!(&r.kind, ResKind::Dir { children } if children.len() > 1))
            .unwrap();
        (ri, 1)
    }

    fn set_child(img: &mut Image, ri: usize, ci: usize, data: Vec<u8>) {
        let ResKind::Dir { children } = &mut img.res[ri].kind else {
            panic!()
        };
        children[ci].data = data;
    }
}
