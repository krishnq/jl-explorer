//! Tools for JieLi BR25 (AC696N/AC636N) speaker firmware: flash image format,
//! file format decoders/encoders, and the USB download-mode protocol.

pub mod app;
pub mod device;
pub mod formats;
pub mod jlfw;
pub mod scsi;
pub mod worker;
