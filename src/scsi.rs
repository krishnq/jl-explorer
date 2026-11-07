//! Minimal SCSI pass-through for USB mass storage devices.
//! Windows: IOCTL_SCSI_PASS_THROUGH_DIRECT on \\.\PhysicalDriveN (needs Administrator).
//! Linux: SG_IO on /dev/sgN (needs read/write access to the sg node, e.g. root).

use std::io;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    None,
    In,
    Out,
}

pub use platform::{candidate_paths, ScsiDevice};

/// Standard INQUIRY: returns (vendor, product, revision).
pub fn inquiry(dev: &mut ScsiDevice) -> io::Result<(String, String, String)> {
    let mut buf = [0u8; 36];
    dev.execute(&[0x12, 0, 0, 0, 36, 0], Dir::In, &mut buf, 5)?;
    let s = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    Ok((s(&buf[8..16]), s(&buf[16..32]), s(&buf[32..36])))
}

#[cfg(windows)]
mod platform {
    use super::Dir;
    use std::ffi::c_void;
    use std::io;
    use std::os::windows::ffi::OsStrExt;

    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 1;
    const FILE_SHARE_WRITE: u32 = 2;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: isize = -1;
    const IOCTL_SCSI_PASS_THROUGH_DIRECT: u32 = 0x4D014;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(name: *const u16, access: u32, share: u32, sa: *mut c_void, disp: u32, flags: u32, template: isize) -> isize;
        fn DeviceIoControl(
            h: isize,
            code: u32,
            inb: *mut c_void,
            inlen: u32,
            outb: *mut c_void,
            outlen: u32,
            ret: *mut u32,
            ov: *mut c_void,
        ) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }

    #[repr(C)]
    struct Sptd {
        length: u16,
        scsi_status: u8,
        path_id: u8,
        target_id: u8,
        lun: u8,
        cdb_length: u8,
        sense_info_length: u8,
        data_in: u8,
        data_transfer_length: u32,
        timeout_value: u32,
        data_buffer: *mut c_void,
        sense_info_offset: u32,
        cdb: [u8; 16],
    }

    #[repr(C)]
    struct SptdWithSense {
        sptd: Sptd,
        sense: [u8; 32],
    }

    pub struct ScsiDevice {
        handle: isize,
    }

    // The handle is only ever used from one thread at a time.
    unsafe impl Send for ScsiDevice {}

    impl ScsiDevice {
        pub fn open(path: &str) -> io::Result<ScsiDevice> {
            let wide: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain([0]).collect();
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    0,
                    0,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(ScsiDevice { handle })
        }

        pub fn execute(&mut self, cdb: &[u8], dir: Dir, buf: &mut [u8], timeout_s: u32) -> io::Result<()> {
            let mut s = SptdWithSense {
                sptd: Sptd {
                    length: std::mem::size_of::<Sptd>() as u16,
                    scsi_status: 0,
                    path_id: 0,
                    target_id: 1,
                    lun: 0,
                    cdb_length: cdb.len() as u8,
                    sense_info_length: 32,
                    data_in: match dir {
                        Dir::Out => 0,
                        Dir::In => 1,
                        Dir::None => 2,
                    },
                    data_transfer_length: if dir == Dir::None { 0 } else { buf.len() as u32 },
                    timeout_value: timeout_s,
                    data_buffer: if dir == Dir::None {
                        std::ptr::null_mut()
                    } else {
                        buf.as_mut_ptr().cast()
                    },
                    sense_info_offset: std::mem::offset_of!(SptdWithSense, sense) as u32,
                    cdb: [0; 16],
                },
                sense: [0; 32],
            };
            s.sptd.cdb[..cdb.len()].copy_from_slice(cdb);
            let size = std::mem::size_of::<SptdWithSense>() as u32;
            let mut returned = 0u32;
            let p = (&mut s as *mut SptdWithSense).cast::<c_void>();
            let ok = unsafe {
                DeviceIoControl(
                    self.handle,
                    IOCTL_SCSI_PASS_THROUGH_DIRECT,
                    p,
                    size,
                    p,
                    size,
                    &mut returned,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for ScsiDevice {
        fn drop(&mut self) {
            unsafe { CloseHandle(self.handle) };
        }
    }

    pub fn candidate_paths() -> Vec<String> {
        (0..32).map(|i| format!(r"\\.\PhysicalDrive{i}")).collect()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::Dir;
    use std::ffi::c_void;
    use std::io;
    use std::os::fd::AsRawFd;

    const SG_IO: u32 = 0x2285;

    #[repr(C)]
    struct SgIoHdr {
        interface_id: i32,
        dxfer_direction: i32,
        cmd_len: u8,
        mx_sb_len: u8,
        iovec_count: u16,
        dxfer_len: u32,
        dxferp: *mut c_void,
        cmdp: *mut u8,
        sbp: *mut u8,
        timeout: u32,
        flags: u32,
        pack_id: i32,
        usr_ptr: *mut c_void,
        status: u8,
        masked_status: u8,
        msg_status: u8,
        sb_len_wr: u8,
        host_status: u16,
        driver_status: u16,
        resid: i32,
        duration: u32,
        info: u32,
    }

    pub struct ScsiDevice {
        file: std::fs::File,
    }

    impl ScsiDevice {
        pub fn open(path: &str) -> io::Result<ScsiDevice> {
            let file = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
            Ok(ScsiDevice { file })
        }

        pub fn execute(&mut self, cdb: &[u8], dir: Dir, buf: &mut [u8], timeout_s: u32) -> io::Result<()> {
            let mut cdb = cdb.to_vec();
            let mut sense = [0u8; 32];
            let mut hdr = SgIoHdr {
                interface_id: 'S' as i32,
                dxfer_direction: match dir {
                    Dir::None => -1,
                    Dir::Out => -2,
                    Dir::In => -3,
                },
                cmd_len: cdb.len() as u8,
                mx_sb_len: sense.len() as u8,
                iovec_count: 0,
                dxfer_len: if dir == Dir::None { 0 } else { buf.len() as u32 },
                dxferp: if dir == Dir::None {
                    std::ptr::null_mut()
                } else {
                    buf.as_mut_ptr().cast()
                },
                cmdp: cdb.as_mut_ptr(),
                sbp: sense.as_mut_ptr(),
                timeout: timeout_s * 1000,
                flags: 0,
                pack_id: 0,
                usr_ptr: std::ptr::null_mut(),
                status: 0,
                masked_status: 0,
                msg_status: 0,
                sb_len_wr: 0,
                host_status: 0,
                driver_status: 0,
                resid: 0,
                duration: 0,
                info: 0,
            };
            let r = unsafe { libc::ioctl(self.file.as_raw_fd(), SG_IO as _, &mut hdr as *mut SgIoHdr) };
            if r < 0 {
                return Err(io::Error::last_os_error());
            }
            if hdr.host_status != 0 || hdr.driver_status & 0x0F != 0 {
                return Err(io::Error::other(format!(
                    "SG_IO failed: host {:#x}, driver {:#x}",
                    hdr.host_status, hdr.driver_status
                )));
            }
            Ok(())
        }
    }

    pub fn candidate_paths() -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir("/dev")
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("sg"))
                    .map(|n| format!("/dev/{n}"))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use super::Dir;
    use std::io;

    pub struct ScsiDevice;

    impl ScsiDevice {
        pub fn open(_path: &str) -> io::Result<ScsiDevice> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "device access is only supported on Windows and Linux",
            ))
        }

        pub fn execute(&mut self, _cdb: &[u8], _dir: Dir, _buf: &mut [u8], _timeout_s: u32) -> io::Result<()> {
            unreachable!()
        }
    }

    pub fn candidate_paths() -> Vec<String> {
        Vec::new()
    }
}
