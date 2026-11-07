//! Background thread that owns the USB device, so the UI never blocks.

use crate::device::{DevInfo, JlDevice, NeedAdmin};
use crate::jlfw::SECTOR;
use std::sync::mpsc::{channel, Receiver, Sender};

pub enum Cmd {
    Connect,
    ReadFlash,
    /// Program `sectors` of `target`, but only if the device still matches `expected` there.
    Write {
        expected: Vec<u8>,
        target: Vec<u8>,
        sectors: Vec<usize>,
    },
    Reboot,
}

pub enum Event {
    Log(String),
    Progress(Option<(f32, String)>),
    Connected(DevInfo),
    Disconnected,
    FlashRead { data: Vec<u8>, after_write: bool },
    WriteDone(Result<(), String>),
    NeedAdmin,
    Error(String),
}

pub struct Worker {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<Event>,
}

impl Worker {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Worker {
        let (tx, cmd_rx) = channel::<Cmd>();
        let (ev_tx, rx) = channel::<Event>();
        std::thread::spawn(move || {
            let send = |e: Event| {
                let _ = ev_tx.send(e);
                repaint();
            };
            let mut dev: Option<JlDevice> = None;
            while let Ok(c) = cmd_rx.recv() {
                match c {
                    Cmd::Connect => {
                        dev = None;
                        send(Event::Progress(Some((0.0, "Connecting…".into()))));
                        match JlDevice::connect(&mut |m| send(Event::Log(m))) {
                            Ok(d) => {
                                send(Event::Connected(d.info.clone()));
                                dev = Some(d);
                                read_flash(dev.as_mut().unwrap(), false, &send);
                            }
                            Err(e) if e.is::<NeedAdmin>() => send(Event::NeedAdmin),
                            Err(e) => send(Event::Error(format!("{e:#}"))),
                        }
                        send(Event::Progress(None));
                    }
                    Cmd::ReadFlash => match dev.as_mut() {
                        Some(d) => read_flash(d, false, &send),
                        None => send(Event::Error("not connected".into())),
                    },
                    Cmd::Write { expected, target, sectors } => {
                        let Some(d) = dev.as_mut() else {
                            send(Event::WriteDone(Err("not connected".into())));
                            continue;
                        };
                        let r = write_sectors(d, &expected, &target, &sectors, &send);
                        let ok = r.is_ok();
                        send(Event::WriteDone(r.map_err(|e| format!("{e:#}"))));
                        // Re-read so the UI reflects what is really in flash now.
                        read_flash(d, ok, &send);
                        send(Event::Progress(None));
                    }
                    Cmd::Reboot => {
                        if let Some(d) = dev.take() {
                            let _ = d.reboot();
                            send(Event::Log("Speaker rebooted into its firmware".into()));
                        }
                        send(Event::Disconnected);
                    }
                }
            }
        });
        Worker { tx, rx }
    }
}

fn read_flash(d: &mut JlDevice, after_write: bool, send: &dyn Fn(Event)) {
    let total = d.info.flash_size;
    let r = d.read(0, total, &mut |n| {
        if n % 0x4000 == 0 || n == total {
            send(Event::Progress(Some((
                n as f32 / total as f32,
                format!("Reading flash {} / {} KB", n / 1024, total / 1024),
            ))));
        }
    });
    send(Event::Progress(None));
    match r {
        Ok(data) => send(Event::FlashRead { data, after_write }),
        Err(e) => send(Event::Error(format!("{e:#}"))),
    }
}

fn write_sectors(d: &mut JlDevice, expected: &[u8], target: &[u8], sectors: &[usize], send: &dyn Fn(Event)) -> anyhow::Result<()> {
    // Make sure nothing changed on the device since the image was read.
    for &s in sectors {
        let cur = d.read(s, SECTOR, &mut |_| {})?;
        if cur != expected[s..s + SECTOR] {
            anyhow::bail!("flash at 0x{s:X} changed since it was read; read the flash again before writing");
        }
    }
    for (i, &s) in sectors.iter().enumerate() {
        send(Event::Progress(Some((
            i as f32 / sectors.len() as f32,
            format!("Writing sector 0x{s:05X} ({}/{})", i + 1, sectors.len()),
        ))));
        d.program_sector(s, &target[s..s + SECTOR])?;
    }
    send(Event::Log(format!("Wrote and verified {} sector(s)", sectors.len())));
    Ok(())
}
