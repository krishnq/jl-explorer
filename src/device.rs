//! JieLi USB download ("UBOOT") protocol over SCSI vendor commands.
//!
//! Flow: the speaker's normal firmware ("UDISK") is asked to reboot into the
//! Boot ROM's UBOOT1.00 mode, a flash loader is uploaded to RAM and started,
//! and then flash is read/erased/written through the loader's command set.
//! Protocol details follow kagaimiq's jl-uboot-tool (MIT).

use crate::jlfw::{crc16, mengli_cipher, SECTOR};
use crate::scsi::{self, Dir, ScsiDevice};
use anyhow::{anyhow, bail, Context, Result};
use std::time::{Duration, Instant};

/// Flash loader for BR25 (AC636N/AC696N/AC608N), from JieLi's SDK (Apache-2.0, see THIRD_PARTY_NOTICES.md).
const BR25_LOADER: &[u8] = include_bytes!("../assets/br25loader.bin");
const BR25_LOADER_ADDR: u32 = 0x12000;
/// Loader argument: target SPI NOR, default clock divider and SPI mode.
const BR25_LOADER_ARG: u16 = 0x0001;

mod cmd {
    // UBOOT1.00 (Boot ROM)
    pub const ROM_WRITE_MEMORY: u16 = 0xFB06;
    pub const ROM_JUMP_TO_MEMORY: u16 = 0xFB08;
    // Loader v2
    pub const ERASE_FLASH_SECTOR: u16 = 0xFB01;
    pub const WRITE_FLASH: u16 = 0xFB04;
    pub const READ_FLASH: u16 = 0xFD05;
    pub const READ_KEY: u16 = 0xFC09;
    pub const GET_ONLINE_DEVICE: u16 = 0xFC0A;
    pub const RUN_APP: u16 = 0xFC0C;
    pub const GET_USB_BUFF_SIZE: u16 = 0xFC14;
}

#[derive(Clone, Debug)]
pub struct Found {
    pub path: String,
    pub vendor: String,
    pub product: String,
}

#[derive(Clone, Debug)]
pub struct DevInfo {
    pub path: String,
    pub chip: String,
    pub flash_id: u32,
    pub flash_size: usize,
    pub chip_key: u16,
    pub io_max: usize,
}

pub struct Scan {
    pub found: Vec<Found>,
    /// At least one disk could not be opened because of missing privileges.
    pub access_denied: bool,
}

/// Look for JieLi devices (disks whose product starts with UBOOT/UDISK/DEVICE).
pub fn scan() -> Scan {
    let mut found = Vec::new();
    let mut access_denied = false;
    for path in scsi::candidate_paths() {
        let mut dev = match ScsiDevice::open(&path) {
            Ok(d) => d,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    access_denied = true;
                }
                continue;
            }
        };
        if let Ok((vendor, product, _)) = scsi::inquiry(&mut dev) {
            if ["UBOOT", "UDISK", "DEVICE"].iter().any(|p| product.starts_with(p)) {
                found.push(Found { path, vendor, product });
            }
        }
    }
    Scan { found, access_denied }
}

fn cdb(cmd: u16, args: &[u8]) -> [u8; 16] {
    let mut c = [0xFFu8; 16];
    c[..2].copy_from_slice(&cmd.to_be_bytes());
    c[2..2 + args.len()].copy_from_slice(args);
    c
}

fn exec(dev: &mut ScsiDevice, cmd: u16, args: &[u8], timeout_s: u32) -> Result<[u8; 14]> {
    let mut resp = [0u8; 16];
    dev.execute(&cdb(cmd, args), Dir::In, &mut resp, timeout_s)?;
    let rcmd = u16::from_be_bytes([resp[0], resp[1]]);
    if rcmd != cmd {
        bail!("unexpected response 0x{rcmd:04X} to command 0x{cmd:04X}");
    }
    Ok(resp[2..].try_into().unwrap())
}

fn mem_write_args(addr: u32, data: &[u8]) -> Vec<u8> {
    let mut a = addr.to_be_bytes().to_vec();
    a.extend_from_slice(&(data.len() as u16).to_be_bytes());
    a.push(0);
    a.extend_from_slice(&crc16(data, 0).to_le_bytes());
    a
}

pub struct JlDevice {
    dev: ScsiDevice,
    pub info: DevInfo,
}

impl JlDevice {
    /// Find the speaker, bring it into download mode, start the loader and
    /// query the flash. `log` receives human readable progress lines.
    pub fn connect(log: &mut dyn FnMut(String)) -> Result<JlDevice> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut asked_reboot = false;
        loop {
            let s = scan();
            if s.found.is_empty() && s.access_denied {
                bail!(NeedAdmin);
            }
            if let Some(f) = s.found.iter().find(|f| f.product.starts_with("UBOOT")) {
                check_chip(f)?;
                log(format!("Found {} {} at {}", f.vendor, f.product, f.path));
                return Self::start_loader(f, log);
            }
            if let Some(f) = s.found.first() {
                check_chip(f)?;
                if !asked_reboot {
                    log(format!(
                        "Found {} {} (normal mode) at {} - asking it to enter download mode",
                        f.vendor, f.product, f.path
                    ));
                    let mut dev = ScsiDevice::open(&f.path)?;
                    // Any loader command makes the SDK firmware reboot into UBOOT1.00.
                    // It is expected to fail as the device drops off the bus.
                    let _ = exec(&mut dev, cmd::GET_ONLINE_DEVICE, &[], 2);
                    asked_reboot = true;
                }
            }
            if Instant::now() > deadline {
                bail!(if asked_reboot {
                    "the speaker did not come back in download mode; unplug it, plug it back in and try again"
                } else {
                    "no JieLi speaker found; is it plugged in over USB?"
                });
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    fn start_loader(f: &Found, log: &mut dyn FnMut(String)) -> Result<JlDevice> {
        let mut last_err = None;
        for attempt in 1..=3 {
            let mut dev = ScsiDevice::open(&f.path).context("opening the download-mode disk")?;
            match upload_loader(&mut dev) {
                Ok(()) => {
                    log("Loader started".into());
                    let info = query_info(&mut dev, &f.path, &f.vendor)?;
                    log(format!(
                        "Flash ID 0x{:06X} ({} KB), chip key 0x{:04X}, I/O block {} bytes",
                        info.flash_id,
                        info.flash_size / 1024,
                        info.chip_key,
                        info.io_max
                    ));
                    return Ok(JlDevice { dev, info });
                }
                Err(e) => {
                    log(format!("Loader upload attempt {attempt} failed: {e:#}"));
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(700));
                }
            }
        }
        Err(last_err.unwrap()).context("could not start the flash loader (try another USB port, away from other USB devices)")
    }

    pub fn read(&mut self, addr: usize, len: usize, progress: &mut dyn FnMut(usize)) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(self.info.io_max);
            let a = (addr + out.len()) as u32;
            let mut args = a.to_be_bytes().to_vec();
            args.extend_from_slice(&(n as u16).to_be_bytes());
            let mut buf = vec![0u8; n];
            self.dev
                .execute(&cdb(cmd::READ_FLASH, &args), Dir::In, &mut buf, 5)
                .with_context(|| format!("reading flash at 0x{a:X}"))?;
            out.extend_from_slice(&buf);
            progress(out.len());
        }
        Ok(out)
    }

    pub fn erase_sector(&mut self, addr: usize) -> Result<()> {
        exec(&mut self.dev, cmd::ERASE_FLASH_SECTOR, &(addr as u32).to_be_bytes(), 10)
            .with_context(|| format!("erasing sector 0x{addr:X}"))?;
        Ok(())
    }

    pub fn write(&mut self, addr: usize, data: &[u8]) -> Result<()> {
        for (i, chunk) in data.chunks(self.info.io_max).enumerate() {
            let a = (addr + i * self.info.io_max) as u32;
            let mut buf = chunk.to_vec();
            self.dev
                .execute(&cdb(cmd::WRITE_FLASH, &mem_write_args(a, chunk)), Dir::Out, &mut buf, 10)
                .with_context(|| format!("writing flash at 0x{a:X}"))?;
        }
        Ok(())
    }

    /// Erase, write and read back one 4K sector.
    pub fn program_sector(&mut self, addr: usize, data: &[u8]) -> Result<()> {
        assert_eq!(data.len(), SECTOR);
        self.erase_sector(addr)?;
        self.write(addr, data)?;
        let back = self.read(addr, SECTOR, &mut |_| {})?;
        if back != data {
            bail!("verify failed at sector 0x{addr:X}");
        }
        Ok(())
    }

    /// Leave download mode and boot the firmware.
    pub fn reboot(mut self) -> Result<()> {
        // The device usually drops off the bus before answering.
        let _ = exec(&mut self.dev, cmd::RUN_APP, &1u32.to_be_bytes(), 2);
        Ok(())
    }
}

/// Marker error: device access needs elevated privileges.
#[derive(Debug)]
pub struct NeedAdmin;

impl std::fmt::Display for NeedAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if cfg!(windows) {
            write!(f, "USB disk access needs Administrator rights")
        } else {
            write!(f, "USB disk access needs root (or read/write access to /dev/sg*)")
        }
    }
}

impl std::error::Error for NeedAdmin {}

fn check_chip(f: &Found) -> Result<()> {
    if f.vendor.eq_ignore_ascii_case("BR25") {
        Ok(())
    } else {
        Err(anyhow!(
            "found a JieLi \"{}\" chip, but only BR25 (AC696N/AC636N) is supported",
            f.vendor
        ))
    }
}

fn upload_loader(dev: &mut ScsiDevice) -> Result<()> {
    // The BR25 Boot ROM accepts the loader blob as stored (already MengLi-encrypted).
    for (i, block) in BR25_LOADER.chunks(512).enumerate() {
        let addr = BR25_LOADER_ADDR + (i * 512) as u32;
        let mut buf = block.to_vec();
        dev.execute(&cdb(cmd::ROM_WRITE_MEMORY, &mem_write_args(addr, block)), Dir::Out, &mut buf, 5)
            .with_context(|| format!("uploading loader block at 0x{addr:X}"))?;
    }
    let mut args = BR25_LOADER_ADDR.to_be_bytes().to_vec();
    args.extend_from_slice(&BR25_LOADER_ARG.to_be_bytes());
    exec(dev, cmd::ROM_JUMP_TO_MEMORY, &args, 5).context("starting the loader")?;
    Ok(())
}

fn query_info(dev: &mut ScsiDevice, path: &str, chip: &str) -> Result<DevInfo> {
    let r = exec(dev, cmd::READ_KEY, &0x00AC_6900u32.to_be_bytes(), 5).context("reading chip key")?;
    let mut k = [r[5], r[4]];
    mengli_cipher(&mut k, 0xFFFF_FFFF);
    let chip_key = u16::from_le_bytes(k);

    let r = exec(dev, cmd::GET_ONLINE_DEVICE, &[], 5).context("querying flash")?;
    let flash_id = u32::from_le_bytes(r[2..6].try_into().unwrap());
    if r[0] != 0x03 {
        bail!("unexpected storage type 0x{:02X} (expected SPI NOR flash)", r[0]);
    }
    let density = flash_id & 0xFF;
    if !(16..=26).contains(&density) {
        bail!("implausible flash ID 0x{flash_id:06X}");
    }
    let flash_size = 1usize << density;

    let io_max = exec(dev, cmd::GET_USB_BUFF_SIZE, &[], 5)
        .ok()
        .map(|r| u32::from_be_bytes(r[0..4].try_into().unwrap()) as usize)
        .filter(|&n| (256..=SECTOR).contains(&n) && SECTOR.is_multiple_of(n))
        .unwrap_or(512);

    Ok(DevInfo {
        path: path.to_string(),
        chip: chip.to_uppercase(),
        flash_id,
        flash_size,
        chip_key,
        io_max,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdb_layout() {
        let c = cdb(cmd::READ_FLASH, &[0, 0, 0x10, 0, 0x02, 0]);
        assert_eq!(&c[..8], &[0xFD, 0x05, 0, 0, 0x10, 0, 0x02, 0]);
        assert!(c[8..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn mem_write_args_layout() {
        let a = mem_write_args(0x12000, b"123456789");
        assert_eq!(a, [0x00, 0x01, 0x20, 0x00, 0x00, 0x09, 0x00, 0xC3, 0x31]);
    }
}
