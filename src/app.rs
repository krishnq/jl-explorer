use crate::device::DevInfo;
use crate::formats::{self, cfgtool, eqcfg, eqpresets, isdcfg, Format};
use crate::jlfw::{self, changed_sectors, dir_required_size, guess_type, Hdr, Image, ResKind, SECTOR};
use crate::worker::{Cmd, Event, Worker};
use eframe::egui::{self, Color32, RichText};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Sel {
    Overview,
    Top(usize),
    Sub(usize),
    Res(usize),
    Child(usize, usize),
}

enum Action {
    Connect,
    Reread,
    OpenImage,
    SaveImage,
    PlanWrite,
    ConfirmWrite,
    CancelWrite,
    Reboot,
    RestartAdmin,
    Replace(Sel),
    Revert(Sel),
    RevertAll,
    Export(Sel),
    OpenExternal(Sel),
    SetData(Sel, Vec<u8>),
    ExportJson(Sel),
    ImportJson(Sel),
}

struct WritePlan {
    target: Vec<u8>,
    sectors: Vec<usize>,
    outside: Vec<usize>,
}

struct ItemView<'a> {
    name: String,
    data: &'a [u8],
    modified: bool,
    abs: Option<usize>,
    hdr: Option<&'a Hdr>,
    editable: bool,
    note: Option<&'static str>,
}

pub struct App {
    worker: Worker,
    image: Option<Image>,
    source: String,
    device_flash: Option<Vec<u8>>,
    dev: Option<DevInfo>,
    busy: bool,
    progress: Option<(f32, String)>,
    log: Vec<String>,
    sel: Sel,
    plan: Option<WritePlan>,
    allow_outside: bool,
    need_admin: bool,
    error: Option<String>,
    last_backup: Option<PathBuf>,
}

const ACCENT: Color32 = Color32::from_rgb(0x4F, 0xA3, 0xF7);
const WARN: Color32 = Color32::from_rgb(0xF2, 0xB1, 0x34);
const BAD: Color32 = Color32::from_rgb(0xEF, 0x5B, 0x5B);
const GOOD: Color32 = Color32::from_rgb(0x5C, 0xC9, 0x7B);

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        let ctx = cc.egui_ctx.clone();
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        ctx.set_style(style);
        ctx.set_visuals(egui::Visuals::dark());
        let repaint = cc.egui_ctx.clone();
        App {
            worker: Worker::spawn(move || repaint.request_repaint()),
            image: None,
            source: String::new(),
            device_flash: None,
            dev: None,
            busy: false,
            progress: None,
            log: vec!["Plug the speaker in over USB and press \"Connect speaker\", or open a flash dump.".into()],
            sel: Sel::Overview,
            plan: None,
            allow_outside: false,
            need_admin: false,
            error: None,
            last_backup: None,
        }
    }

    fn log(&mut self, s: impl Into<String>) {
        self.log.push(s.into());
        if self.log.len() > 500 {
            self.log.drain(..100);
        }
    }

    fn handle_events(&mut self) {
        while let Ok(ev) = self.worker.rx.try_recv() {
            match ev {
                Event::Log(m) => self.log(m),
                Event::Progress(p) => self.progress = p,
                Event::Connected(info) => {
                    self.log(format!("Connected: {} at {}", info.chip, info.path));
                    self.need_admin = false;
                    self.dev = Some(info);
                }
                Event::Disconnected => {
                    self.dev = None;
                    self.device_flash = None;
                    self.busy = false;
                }
                Event::NeedAdmin => {
                    self.need_admin = true;
                    self.busy = false;
                }
                Event::Error(e) => {
                    self.log(format!("Error: {e}"));
                    self.error = Some(e);
                    self.busy = false;
                }
                Event::WriteDone(r) => match r {
                    Ok(()) => self.log("Write complete and verified."),
                    Err(e) => {
                        self.log(format!("Write failed: {e}"));
                        self.error = Some(format!("Write failed: {e}\nYour backup is at {}", self.backup_hint()));
                    }
                },
                Event::FlashRead { data, after_write } => {
                    self.busy = false;
                    match save_backup(&data) {
                        Ok(p) => {
                            self.log(format!("Backup of the flash saved to {}", p.display()));
                            self.last_backup = Some(p);
                        }
                        Err(e) => self.log(format!("Could not save a backup: {e}")),
                    }
                    let keep_edits = !after_write && self.image.as_ref().is_some_and(Image::is_modified);
                    if keep_edits {
                        self.log("Flash re-read; your unsaved edits were kept.");
                    } else {
                        let key = self.dev.as_ref().map(|d| d.chip_key);
                        match Image::parse(data.clone(), key) {
                            Ok(img) => {
                                self.source = format!("speaker ({})", self.dev.as_ref().map(|d| d.path.as_str()).unwrap_or("?"));
                                self.image = Some(img);
                                self.sel = Sel::Overview;
                            }
                            Err(e) => self.error = Some(format!("Read the flash, but could not parse it: {e:#}")),
                        }
                    }
                    self.device_flash = Some(data);
                }
            }
        }
    }

    fn backup_hint(&self) -> String {
        self.last_backup
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none saved)".into())
    }

    fn item(&self, sel: Sel) -> Option<ItemView<'_>> {
        let img = self.image.as_ref()?;
        Some(match sel {
            Sel::Overview => return None,
            Sel::Top(i) => {
                let t = img.top.get(i)?;
                ItemView {
                    name: t.hdr.name(),
                    data: img.top_data(i),
                    modified: false,
                    abs: Some(t.data_abs),
                    hdr: Some(&t.hdr),
                    editable: false,
                    note: Some(if t.hdr.name() == "uboot.boot" {
                        "Second-stage bootloader (shown as stored, still scrambled). Read-only."
                    } else {
                        "Boot area entry. Read-only."
                    }),
                }
            }
            Sel::Sub(i) => {
                let s = img.app_subs.get(i)?;
                ItemView {
                    name: s.hdr.name(),
                    data: img.app_sub_data(i),
                    modified: false,
                    abs: Some(s.abs),
                    hdr: Some(&s.hdr),
                    editable: false,
                    note: Some(if s.hdr.name() == "app.bin" {
                        "Main program code (decrypted). Read-only here; export it to study in a disassembler such as Ghidra."
                    } else {
                        "Reserved area (settings, pairing data, protection range). Read-only."
                    }),
                }
            }
            Sel::Res(i) => {
                let r = img.res.get(i)?;
                match &r.kind {
                    ResKind::File { orig, data } => ItemView {
                        name: r.name(),
                        data,
                        modified: orig != data,
                        abs: Some(r.orig_abs + 32),
                        hdr: Some(&r.hdr),
                        editable: true,
                        note: Some("Changing this file's size moves the entries after it."),
                    },
                    ResKind::Dir { .. } => ItemView {
                        name: r.name() + "/",
                        data: &img.dec[r.orig_abs..r.orig_abs + r.hdr.size as usize],
                        modified: r.is_modified(),
                        abs: Some(r.orig_abs),
                        hdr: Some(&r.hdr),
                        editable: false,
                        note: Some("Directory (original contents shown). Select a file inside it to replace it."),
                    },
                    ResKind::Raw { bytes } => ItemView {
                        name: r.name(),
                        data: bytes,
                        modified: false,
                        abs: Some(r.orig_abs),
                        hdr: Some(&r.hdr),
                        editable: false,
                        note: Some("Unrecognised entry, kept as-is."),
                    },
                }
            }
            Sel::Child(i, j) => {
                let r = img.res.get(i)?;
                let ResKind::Dir { children } = &r.kind else { return None };
                let c = children.get(j)?;
                ItemView {
                    name: c.hdr.name(),
                    data: &c.data,
                    modified: c.data != c.orig,
                    abs: Some(r.orig_abs + c.hdr.offset as usize),
                    hdr: Some(&c.hdr),
                    editable: c.hdr.name() != "index.idx",
                    note: if c.hdr.name() == "index.idx" {
                        Some("Tone index used by the firmware to find prompts. Kept as-is.")
                    } else {
                        None
                    },
                }
            }
        })
    }

    /// Original (as read) contents of an editable file.
    fn orig_of(&self, sel: Sel) -> Option<&[u8]> {
        let img = self.image.as_ref()?;
        match sel {
            Sel::Res(i) => match &img.res.get(i)?.kind {
                ResKind::File { orig, .. } => Some(orig),
                _ => None,
            },
            Sel::Child(i, j) => match &img.res.get(i)?.kind {
                ResKind::Dir { children } => children.get(j).map(|c| c.orig.as_slice()),
                _ => None,
            },
            _ => None,
        }
    }

    fn set_data(&mut self, sel: Sel, new: Option<Vec<u8>>) {
        let Some(img) = self.image.as_mut() else { return };
        match sel {
            Sel::Res(i) => {
                if let Some(ResKind::File { orig, data }) = img.res.get_mut(i).map(|r| &mut r.kind) {
                    *data = new.unwrap_or_else(|| orig.clone());
                }
            }
            Sel::Child(i, j) => {
                if let Some(ResKind::Dir { children }) = img.res.get_mut(i).map(|r| &mut r.kind) {
                    if let Some(c) = children.get_mut(j) {
                        c.data = new.unwrap_or_else(|| c.orig.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn apply(&mut self, a: Action) {
        match a {
            Action::Connect => {
                self.busy = true;
                self.error = None;
                let _ = self.worker.tx.send(Cmd::Connect);
            }
            Action::Reread => {
                self.busy = true;
                let _ = self.worker.tx.send(Cmd::ReadFlash);
            }
            Action::Reboot => {
                let _ = self.worker.tx.send(Cmd::Reboot);
            }
            Action::OpenImage => {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Flash image", &["bin"])
                    .add_filter("All files", &["*"])
                    .pick_file()
                {
                    match std::fs::read(&p)
                        .map_err(anyhow::Error::from)
                        .and_then(|raw| Image::parse(raw, self.dev.as_ref().map(|d| d.chip_key)))
                    {
                        Ok(img) => {
                            self.log(format!("Opened {} (chip key 0x{:04X})", p.display(), img.chipkey));
                            self.source = p.display().to_string();
                            self.image = Some(img);
                            self.sel = Sel::Overview;
                        }
                        Err(e) => self.error = Some(format!("Could not open {}: {e:#}", p.display())),
                    }
                }
            }
            Action::SaveImage => {
                let Some(img) = &self.image else { return };
                match img.build() {
                    Ok(b) => {
                        if let Some(p) = rfd::FileDialog::new()
                            .set_file_name("flash_modified.bin")
                            .add_filter("Flash image", &["bin"])
                            .save_file()
                        {
                            match std::fs::write(&p, &b.image) {
                                Ok(()) => self.log(format!("Saved image to {}", p.display())),
                                Err(e) => self.error = Some(format!("Save failed: {e}")),
                            }
                        }
                    }
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
            Action::PlanWrite => {
                let (Some(img), Some(flash)) = (&self.image, &self.device_flash) else {
                    return;
                };
                let built = match img.build() {
                    Ok(b) => b.image,
                    Err(e) => {
                        self.error = Some(format!("{e:#}"));
                        return;
                    }
                };
                if built.len() != flash.len() {
                    self.error = Some(format!(
                        "Image is {} KB but the speaker's flash is {} KB",
                        built.len() / 1024,
                        flash.len() / 1024
                    ));
                    return;
                }
                let sectors = changed_sectors(flash, &built);
                if sectors.is_empty() {
                    self.log("Nothing to write: the speaker already has this image.");
                    return;
                }
                let lo = img.appbase & !(SECTOR - 1);
                let outside = sectors.iter().copied().filter(|&s| s < lo || s + SECTOR > img.app_limit).collect();
                self.allow_outside = false;
                self.plan = Some(WritePlan {
                    target: built,
                    sectors,
                    outside,
                });
            }
            Action::ConfirmWrite => {
                if let (Some(plan), Some(flash)) = (self.plan.take(), &self.device_flash) {
                    self.busy = true;
                    self.error = None;
                    let _ = self.worker.tx.send(Cmd::Write {
                        expected: flash.clone(),
                        target: plan.target,
                        sectors: plan.sectors,
                    });
                }
            }
            Action::CancelWrite => self.plan = None,
            Action::RestartAdmin => restart_as_admin(),
            Action::Replace(sel) => {
                let name = self.item(sel).map(|v| v.name).unwrap_or_default();
                let ext = name.rsplit('.').next().unwrap_or("").to_string();
                let mut dlg = rfd::FileDialog::new().set_title(format!("Replace {name}"));
                if !ext.is_empty() && ext != name {
                    dlg = dlg.add_filter(&ext, &[&ext]);
                }
                if let Some(p) = dlg.add_filter("All files", &["*"]).pick_file() {
                    match std::fs::read(&p) {
                        Ok(d) => {
                            self.log(format!("Replaced {name} with {} ({} bytes)", p.display(), d.len()));
                            self.set_data(sel, Some(d));
                        }
                        Err(e) => self.error = Some(format!("{e}")),
                    }
                }
            }
            Action::Revert(sel) => self.set_data(sel, None),
            Action::RevertAll => {
                if let Some(img) = self.image.as_mut() {
                    for r in &mut img.res {
                        match &mut r.kind {
                            ResKind::File { orig, data } => *data = orig.clone(),
                            ResKind::Dir { children } => children.iter_mut().for_each(|c| c.data = c.orig.clone()),
                            ResKind::Raw { .. } => {}
                        }
                    }
                }
            }
            Action::Export(sel) => {
                if let Some(v) = self.item(sel) {
                    let fname = v.name.trim_end_matches('/').to_string();
                    let data = v.data.to_vec();
                    if let Some(p) = rfd::FileDialog::new().set_file_name(&fname).save_file() {
                        match std::fs::write(&p, data) {
                            Ok(()) => self.log(format!("Exported {fname} to {}", p.display())),
                            Err(e) => self.error = Some(format!("{e}")),
                        }
                    }
                }
            }
            Action::SetData(sel, data) => self.set_data(sel, Some(data)),
            Action::ExportJson(sel) => {
                let Some(v) = self.item(sel) else { return };
                let r = Format::detect(&v.name, v.data)
                    .ok_or_else(|| anyhow::anyhow!("no decoder for {}", v.name))
                    .and_then(|f| formats::decompile(f, v.data));
                match r {
                    Ok(j) => {
                        let fname = format!("{}.json", v.name.rsplit_once('.').map(|p| p.0).unwrap_or(&v.name));
                        if let Some(p) = rfd::FileDialog::new()
                            .set_file_name(&fname)
                            .add_filter("JSON", &["json"])
                            .save_file()
                        {
                            let text = serde_json::to_string_pretty(&j).unwrap_or_default() + "\n";
                            match std::fs::write(&p, text) {
                                Ok(()) => self.log(format!("Decompiled {} to {}", v.name, p.display())),
                                Err(e) => self.error = Some(format!("{e}")),
                            }
                        }
                    }
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
            Action::ImportJson(sel) => {
                if let Some(p) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                    let r = std::fs::read_to_string(&p)
                        .map_err(anyhow::Error::from)
                        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(anyhow::Error::from))
                        .and_then(|j| formats::compile(&j));
                    match r {
                        Ok(bin) => {
                            self.log(format!("Compiled {} ({} bytes)", p.display(), bin.len()));
                            self.set_data(sel, Some(bin));
                        }
                        Err(e) => self.error = Some(format!("Could not compile {}: {e:#}", p.display())),
                    }
                }
            }
            Action::OpenExternal(sel) => {
                if let Some(v) = self.item(sel) {
                    let dir = std::env::temp_dir().join("jl-explorer");
                    let path = dir.join(v.name.trim_end_matches('/'));
                    let r = std::fs::create_dir_all(&dir)
                        .and_then(|_| std::fs::write(&path, v.data))
                        .map_err(anyhow::Error::from)
                        .and_then(|_| open::that(&path).map_err(anyhow::Error::from));
                    if let Err(e) = r {
                        self.error = Some(format!("Could not open {}: {e}", path.display()));
                    }
                }
            }
        }
    }

    fn top_bar(&self, ui: &mut egui::Ui, act: &mut Vec<Action>) {
        ui.horizontal_wrapped(|ui| {
            ui.heading(RichText::new("JL Explorer").color(ACCENT).strong());
            ui.separator();
            let idle = !self.busy;
            if ui
                .add_enabled(idle, egui::Button::new("🔌 Connect speaker"))
                .on_hover_text("Find the speaker on USB, enter download mode and read the whole flash")
                .clicked()
            {
                act.push(Action::Connect);
            }
            if ui
                .add_enabled(idle && self.dev.is_some(), egui::Button::new("⟳ Re-read flash"))
                .clicked()
            {
                act.push(Action::Reread);
            }
            if ui.add_enabled(idle, egui::Button::new("📂 Open dump…")).clicked() {
                act.push(Action::OpenImage);
            }
            if ui
                .add_enabled(self.image.is_some(), egui::Button::new("💾 Save image…"))
                .on_hover_text("Save the image with your edits applied")
                .clicked()
            {
                act.push(Action::SaveImage);
            }
            ui.separator();
            let can_write = idle && self.dev.is_some() && self.device_flash.is_some() && self.image.is_some();
            let wbtn = egui::Button::new(RichText::new("⬆ Write to speaker").strong()).fill(if can_write {
                Color32::from_rgb(0x1F, 0x5F, 0x9E)
            } else {
                Color32::TRANSPARENT
            });
            if ui
                .add_enabled(can_write, wbtn)
                .on_hover_text("Erase and rewrite only the 4K sectors that differ, then verify")
                .clicked()
            {
                act.push(Action::PlanWrite);
            }
            if ui
                .add_enabled(idle && self.dev.is_some(), egui::Button::new("⏻ Reboot speaker"))
                .on_hover_text("Leave download mode and start the speaker's firmware")
                .clicked()
            {
                act.push(Action::Reboot);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &self.dev {
                Some(d) => {
                    ui.label(
                        RichText::new(format!(
                            "● {}  ·  {} KB flash  ·  key {:04X}",
                            d.chip,
                            d.flash_size / 1024,
                            d.chip_key
                        ))
                        .color(GOOD),
                    );
                }
                None => {
                    ui.label(RichText::new("○ not connected").weak());
                }
            });
        });
    }

    fn tree(&self, ui: &mut egui::Ui, new_sel: &mut Option<Sel>) {
        let Some(img) = &self.image else {
            ui.label(RichText::new("No image loaded").weak());
            return;
        };
        let mut item = |ui: &mut egui::Ui, sel: Sel, label: String, modified: bool| {
            let text = if modified {
                RichText::new(format!("{label}  ●")).color(WARN)
            } else {
                RichText::new(label)
            };
            if ui.selectable_label(self.sel == sel, text).clicked() {
                *new_sel = Some(sel);
            }
        };
        item(ui, Sel::Overview, "ℹ Overview".into(), img.is_modified());
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Boot area").default_open(false).show(ui, |ui| {
            for (i, t) in img.top.iter().enumerate() {
                if t.hdr.name() != "app_dir_head" {
                    item(ui, Sel::Top(i), format!("🔒 {}", t.hdr.name()), false);
                }
            }
        });
        egui::CollapsingHeader::new("App area").default_open(true).show(ui, |ui| {
            for (i, s) in img.app_subs.iter().enumerate() {
                item(ui, Sel::Sub(i), format!("🔒 {}", s.hdr.name()), false);
            }
            for (i, r) in img.res.iter().enumerate() {
                match &r.kind {
                    ResKind::Dir { children } => {
                        egui::CollapsingHeader::new(RichText::new(format!("📁 {}", r.name())).color(if r.is_modified() {
                            WARN
                        } else {
                            ui.visuals().text_color()
                        }))
                        .id_salt(("dir", i))
                        .default_open(true)
                        .show(ui, |ui| {
                            item(ui, Sel::Res(i), "(directory info)".into(), false);
                            for (j, c) in children.iter().enumerate() {
                                let icon = if guess_type(&c.hdr.name(), &c.data) == "MP3 audio" {
                                    "🔊"
                                } else {
                                    "📄"
                                };
                                item(ui, Sel::Child(i, j), format!("{icon} {}", c.hdr.name()), c.data != c.orig);
                            }
                        });
                    }
                    ResKind::File { .. } => item(ui, Sel::Res(i), format!("📄 {}", r.name()), r.is_modified()),
                    ResKind::Raw { .. } => item(ui, Sel::Res(i), format!("❔ {}", r.name()), false),
                }
            }
        });
    }

    fn overview(&self, ui: &mut egui::Ui, act: &mut Vec<Action>) {
        let Some(img) = &self.image else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("Connect your speaker or open a flash dump");
                ui.label(RichText::new("The app reads the whole flash, makes a backup, and lets you browse and replace files.").weak());
            });
            return;
        };
        ui.heading(RichText::new(&img.header.pid).strong());
        egui::Grid::new("ov")
            .num_columns(2)
            .spacing([24.0, 6.0])
            .striped(true)
            .show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    ui.label(RichText::new(k).weak());
                    ui.label(RichText::new(v).monospace());
                    ui.end_row();
                };
                row("Source", self.source.clone());
                row("Firmware version", img.header.vid.clone());
                row(
                    "Image size",
                    format!(
                        "{} KB (header says {} KB in use)",
                        img.raw.len() / 1024,
                        img.header.flash_size / 1024
                    ),
                );
                row("Chip key", format!("0x{:04X}", img.chipkey));
                row(
                    "App area",
                    format!(
                        "0x{:06X} – 0x{:06X} (may grow to 0x{:06X})",
                        img.appbase, img.app_end, img.app_limit
                    ),
                );
                if let Some(p) = &self.last_backup {
                    row("Last backup", p.display().to_string());
                }
            });
        ui.add_space(8.0);
        let end = img.built_app_end();
        let span = (img.app_limit - img.appbase) as f32;
        let frac = (end - img.appbase) as f32 / span;
        ui.label("App area usage after your edits");
        ui.add(
            egui::ProgressBar::new(frac.min(1.0))
                .text(format!(
                    "{} KB of {} KB",
                    (end - img.appbase) / 1024,
                    (img.app_limit - img.appbase) / 1024
                ))
                .fill(if end > img.app_limit { BAD } else { ACCENT }),
        );
        if end > img.app_limit {
            ui.colored_label(BAD, "Your edits do not fit. Use smaller files.");
        } else if end != img.app_end {
            ui.colored_label(
                WARN,
                "Some entries will move because a file changed size. That is normal, but prefer same-or-smaller files where possible.",
            );
        }

        ui.add_space(10.0);
        ui.label(RichText::new("Pending changes").strong());
        let mut any = false;
        for r in &img.res {
            match &r.kind {
                ResKind::File { orig, data } if orig != data => {
                    any = true;
                    ui.label(format!("• {}: {} → {} bytes", r.name(), orig.len(), data.len()));
                }
                ResKind::Dir { children } => {
                    for c in children.iter().filter(|c| c.data != c.orig) {
                        any = true;
                        ui.label(format!(
                            "• {}/{}: {} → {} bytes",
                            r.name(),
                            c.hdr.name(),
                            c.orig.len(),
                            c.data.len()
                        ));
                    }
                }
                _ => {}
            }
        }
        if any {
            if ui.button("↺ Revert all").clicked() {
                act.push(Action::RevertAll);
            }
        } else {
            ui.label(RichText::new("None. Select a file on the left and use Replace…").weak());
        }
    }

    fn details(&self, ui: &mut egui::Ui, act: &mut Vec<Action>) {
        if self.sel == Sel::Overview {
            self.overview(ui, act);
            return;
        }
        let Some(v) = self.item(self.sel) else { return };
        ui.horizontal(|ui| {
            ui.heading(RichText::new(&v.name).strong());
            if v.modified {
                ui.label(RichText::new("modified").color(WARN));
            }
        });
        egui::Grid::new("info").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
            let mut row = |k: &str, val: String| {
                ui.label(RichText::new(k).weak());
                ui.label(RichText::new(val).monospace());
                ui.end_row();
            };
            row("Type", guess_type(&v.name, v.data).to_string());
            row("Size", format!("{} bytes", v.data.len()));
            if let Some(a) = v.abs {
                row("Flash offset", format!("0x{a:06X} (original)"));
            }
            row("CRC16", format!("0x{:04X}", jlfw::crc16(v.data, 0)));
            if let Some(h) = v.hdr {
                row("Entry flags", format!("0x{:02X}", h.flags));
            }
        });
        if let Some(n) = v.note {
            ui.label(RichText::new(n).weak().italics());
        }
        if let Sel::Child(i, _) = self.sel {
            if let Some(img) = &self.image {
                let r = &img.res[i];
                if let ResKind::Dir { children } = &r.kind {
                    let need = dir_required_size(children);
                    let slot = r.hdr.size as usize;
                    let text = format!("{}/ uses {} of {} bytes", r.name(), need, slot);
                    ui.add(
                        egui::ProgressBar::new((need as f32 / slot as f32).min(1.0))
                            .text(text)
                            .fill(if need > slot { WARN } else { ACCENT }),
                    );
                    if need > slot {
                        ui.colored_label(WARN, "Larger than the original directory: entries after it will move.");
                    }
                }
            }
        }
        ui.horizontal(|ui| {
            if v.editable && ui.button("📥 Replace…").clicked() {
                act.push(Action::Replace(self.sel));
            }
            if v.modified && ui.button("↺ Revert").clicked() {
                act.push(Action::Revert(self.sel));
            }
            if ui.button("📤 Export…").clicked() {
                act.push(Action::Export(self.sel));
            }
            if ui.button("▶ Open in default app").on_hover_text("e.g. play an MP3").clicked() {
                act.push(Action::OpenExternal(self.sel));
            }
        });
        let fmt = Format::detect(&v.name, v.data);
        if let Some(fmt) = fmt {
            ui.horizontal(|ui| {
                if ui
                    .button("🧾 Export JSON…")
                    .on_hover_text("Decompile to an editable JSON file")
                    .clicked()
                {
                    act.push(Action::ExportJson(self.sel));
                }
                if v.editable
                    && fmt != Format::IsdConfig
                    && ui
                        .button("🧩 Import JSON…")
                        .on_hover_text("Compile an edited JSON file back into this file")
                        .clicked()
                {
                    act.push(Action::ImportJson(self.sel));
                }
            });
        }
        ui.separator();
        match fmt {
            Some(fmt) => {
                let sel = self.sel;
                egui::ScrollArea::vertical()
                    .id_salt(("decoded", format!("{sel:?}")))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let edited = match fmt {
                            Format::CfgTool => cfg_editor(ui, v.data),
                            Format::EqCfg => eq_editor(ui, v.data, self.orig_of(sel)),
                            Format::IsdConfig => {
                                isd_view(ui, v.data);
                                None
                            }
                        };
                        if let Some(d) = edited.filter(|_| v.editable) {
                            act.push(Action::SetData(sel, d));
                        }
                        ui.add_space(8.0);
                        egui::CollapsingHeader::new("Raw bytes")
                            .default_open(false)
                            .show(ui, |ui| hex_view(ui, v.data, 320.0));
                    });
            }
            None => hex_view(ui, v.data, f32::INFINITY),
        }
    }

    fn write_dialog(&mut self, ctx: &egui::Context, act: &mut Vec<Action>) {
        let Some(plan) = &self.plan else { return };
        let mut allow = self.allow_outside;
        egui::Window::new("Write to speaker")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} sector(s), {} KB, will be erased, rewritten and verified:",
                    plan.sectors.len(),
                    plan.sectors.len() * SECTOR / 1024
                ));
                ui.label(RichText::new(ranges(&plan.sectors)).monospace());
                ui.label(format!("Backup of the current flash: {}", self.backup_hint()));
                if !plan.outside.is_empty() {
                    ui.add_space(6.0);
                    ui.colored_label(
                        BAD,
                        format!(
                            "⚠ {} sector(s) lie outside the app area (bootloader / settings): {}",
                            plan.outside.len(),
                            ranges(&plan.outside)
                        ),
                    );
                    ui.colored_label(BAD, "Only allow this when restoring a known-good full backup of this same speaker.");
                    ui.checkbox(&mut allow, "I understand, allow writing outside the app area");
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let ok = plan.outside.is_empty() || allow;
                    if ui.add_enabled(ok, egui::Button::new(RichText::new("Write").strong())).clicked() {
                        act.push(Action::ConfirmWrite);
                    }
                    if ui.button("Cancel").clicked() {
                        act.push(Action::CancelWrite);
                    }
                });
            });
        self.allow_outside = allow;
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_events();
        let mut act = Vec::new();
        let mut new_sel = None;

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui, &mut act);
            if self.need_admin {
                ui.horizontal(|ui| {
                    ui.colored_label(WARN, format!("⚠ {}", crate::device::NeedAdmin));
                    if cfg!(windows) && ui.button("Restart as Administrator").clicked() {
                        act.push(Action::RestartAdmin);
                    }
                });
            }
            if let Some(e) = self.error.clone() {
                ui.horizontal(|ui| {
                    ui.colored_label(BAD, format!("✖ {e}"));
                    if ui.small_button("dismiss").clicked() {
                        self.error = None;
                    }
                });
            }
            ui.add_space(2.0);
        });

        egui::TopBottomPanel::bottom("bottom")
            .resizable(true)
            .default_height(120.0)
            .show(ctx, |ui| {
                if let Some((p, t)) = &self.progress {
                    ui.add(egui::ProgressBar::new(*p).text(t.as_str()).animate(true));
                }
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        for l in &self.log {
                            let c = if l.starts_with("Error") || l.starts_with("Write failed") {
                                BAD
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(RichText::new(l).monospace().size(12.0).color(c));
                        }
                    });
            });

        egui::SidePanel::left("tree").resizable(true).default_width(250.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.tree(ui, &mut new_sel));
        });

        egui::CentralPanel::default().show(ctx, |ui| self.details(ui, &mut act));

        self.write_dialog(ctx, &mut act);

        if let Some(s) = new_sel {
            self.sel = s;
        }
        for a in act {
            self.apply(a);
        }
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

/// Settings editor for cfg_tool.bin. Returns the re-encoded file when a value changed.
fn cfg_editor(ui: &mut egui::Ui, data: &[u8]) -> Option<Vec<u8>> {
    let mut cfg = match cfgtool::parse(data) {
        Ok(c) => c,
        Err(e) => {
            ui.colored_label(BAD, format!("Could not decode: {e:#}"));
            return None;
        }
    };
    ui.label(
        RichText::new("Speaker settings (cfg_tool.bin). Field names come from JieLi's SDK; values are written back with fresh checksums.")
            .weak(),
    );
    let mut changed = false;
    for (ii, item) in cfg.items.iter_mut().enumerate() {
        let warn = if item.checksum_ok { "" } else { "  ⚠ checksum" };
        let title = format!(
            "{}  ·  id {}  ·  {} bytes{warn}",
            cfgtool::item_name(item.id),
            item.id,
            item.data.len()
        );
        egui::CollapsingHeader::new(title)
            .id_salt(("cfg", ii))
            .default_open(matches!(item.id, 524 | 603))
            .show(ui, |ui| match cfgtool::schema(item.id, item.data.len()) {
                None => {
                    ui.label(RichText::new(cfgtool::hex(&item.data)).monospace());
                    ui.label(RichText::new("Not decoded yet").weak());
                }
                Some(fields) => {
                    egui::Grid::new(("cfg_grid", ii)).num_columns(2).striped(true).show(ui, |ui| {
                        for fl in &fields {
                            ui.label(&fl.label);
                            if let Some(v) = field_widget(ui, &item.data, fl) {
                                changed |= cfgtool::write_field(&mut item.data, fl, &v).is_ok();
                            }
                            ui.end_row();
                        }
                    });
                }
            });
    }
    changed.then(|| cfgtool::encode(&cfg))
}

fn field_widget(ui: &mut egui::Ui, data: &[u8], fl: &cfgtool::Field) -> Option<Value> {
    use cfgtool::Kind;
    let cur = cfgtool::read_field(data, fl);
    match fl.kind {
        Kind::U8 => {
            let mut x = data[fl.offset];
            ui.add(egui::DragValue::new(&mut x)).changed().then(|| json!(x))
        }
        Kind::U16 => {
            let mut x = u16::from_le_bytes([data[fl.offset], data[fl.offset + 1]]);
            ui.add(egui::DragValue::new(&mut x)).changed().then(|| json!(x))
        }
        Kind::F32 => {
            let mut x = cur.as_f64().unwrap_or(0.0) as f32;
            ui.add(egui::DragValue::new(&mut x).speed(0.01)).changed().then(|| json!(x))
        }
        Kind::Text(n) => {
            let mut s = cur.as_str().unwrap_or_default().to_string();
            let r = ui
                .add(egui::TextEdit::singleline(&mut s).desired_width(260.0))
                .on_hover_text(format!("up to {n} bytes"));
            (r.changed() && s.len() <= n).then(|| json!(s))
        }
        Kind::U16Hex | Kind::Mac => {
            let mut s = cur.as_str().unwrap_or_default().to_string();
            ui.add(
                egui::TextEdit::singleline(&mut s)
                    .desired_width(160.0)
                    .font(egui::TextStyle::Monospace),
            )
            .changed()
            .then(|| json!(s))
        }
    }
}

/// EQ editor for eq_cfg_hw.bin. Returns the re-encoded file when a value changed.
/// `orig` is the file as read from the speaker; presets are built on its tuning.
fn eq_editor(ui: &mut egui::Ui, data: &[u8], orig: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut f = match eqcfg::parse(data) {
        Ok(f) => f,
        Err(e) => {
            ui.colored_label(BAD, format!("Could not decode: {e:#}"));
            return None;
        }
    };
    let stock_of = |id: u16| -> Option<eqcfg::EqTable> {
        let o = eqcfg::parse(orig?).ok()?;
        o.groups
            .iter()
            .flat_map(|g| &g.chunks)
            .find(|c| c.id == id)
            .and_then(|c| eqcfg::EqTable::parse(&c.data))
    };
    ui.label(
        RichText::new("EQ presets (eq_cfg_hw.bin). Drag values to change them; checksums and the directory are rebuilt on save.").weak(),
    );
    let names = eqcfg::mode_names(&f);
    let mut changed = false;
    for (gi, g) in f.groups.iter_mut().enumerate() {
        let name = names
            .get(gi)
            .cloned()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Group {}", g.id));
        egui::CollapsingHeader::new(RichText::new(name).strong()).id_salt(("eq_group", gi)).default_open(gi == 0).show(ui, |ui| {
            for (ci, c) in g.chunks.iter_mut().enumerate() {
                ui.push_id((gi, ci), |ui| {
                    ui.label(RichText::new(format!("{}  ·  0x{:04X}", eqcfg::chunk_label(c.id), c.id)).color(ACCENT));
                    let Some(mut t) = eqcfg::EqTable::parse(&c.data) else {
                        ui.label(RichText::new(format!("{} bytes, not decoded yet", c.data.len())).weak());
                        return;
                    };
                    let mut tch = false;
                    if t.bands.len() == 10 {
                        let stock = stock_of(c.id).unwrap_or_else(|| t.clone());
                        ui.label(RichText::new("Presets").strong());
                        ui.horizontal_wrapped(|ui| {
                            for p in eqpresets::PRESETS {
                                let built = eqpresets::apply(&stock, p);
                                let active = built == t;
                                let text = if active { RichText::new(format!("✔ {}", p.name)).strong() } else { RichText::new(p.name) };
                                let btn = egui::Button::new(text).selected(active).min_size(egui::vec2(0.0, 26.0));
                                if ui.add(btn).on_hover_text(format!("{}\nglobal gain {:+.1} dB", p.description, built.global_gain)).clicked() && !active {
                                    t = built;
                                    tch = true;
                                }
                            }
                        });
                        ui.label(RichText::new("Built on the original tuning. Global gain keeps the overall peak no higher than stock. Fine-tune the bands below after picking one.").weak().small());
                        ui.add_space(4.0);
                    }
                    let eqcfg::EqTable { global_gain, enable_mask, bands } = &mut t;
                    ui.horizontal(|ui| {
                        ui.label("Global gain (dB)");
                        tch |= ui.add(egui::DragValue::new(global_gain).speed(0.05).range(-24.0..=24.0).fixed_decimals(2)).changed();
                    });
                    egui::Grid::new("bands").num_columns(6).striped(true).show(ui, |ui| {
                        for h in ["On", "#", "Type", "Freq (Hz)", "Gain (dB)", "Q"] {
                            ui.label(RichText::new(h).weak());
                        }
                        ui.end_row();
                        for (bi, b) in bands.iter_mut().enumerate() {
                            let mut on = *enable_mask & (1 << bi) != 0;
                            if ui.checkbox(&mut on, "").changed() {
                                *enable_mask ^= 1 << bi;
                                tch = true;
                            }
                            ui.label(b.index.to_string());
                            egui::ComboBox::from_id_salt(("type", bi)).selected_text(eqcfg::filter_name(b.filter)).show_ui(ui, |ui| {
                                for (k, n) in eqcfg::FILTER_NAMES.iter().enumerate() {
                                    tch |= ui.selectable_value(&mut b.filter, k as u16, *n).changed();
                                }
                            });
                            tch |= ui.add(egui::DragValue::new(&mut b.freq).speed(5.0).range(10..=24000)).changed();
                            tch |= ui.add(egui::DragValue::new(&mut b.gain).speed(0.05).range(-12.0..=12.0).fixed_decimals(2)).changed();
                            tch |= ui.add(egui::DragValue::new(&mut b.q).speed(0.01).range(0.3..=30.0).fixed_decimals(2)).changed();
                            ui.end_row();
                        }
                    });
                    eq_curve(ui, &t);
                    if tch {
                        c.data = t.encode();
                        changed = true;
                    }
                    ui.add_space(6.0);
                });
            }
        });
    }
    changed.then(|| eqcfg::encode(&f))
}

/// Approximate frequency response of an EQ table (20 Hz – 20 kHz, −18 … +12 dB).
fn eq_curve(ui: &mut egui::Ui, t: &eqcfg::EqTable) {
    let (lo_db, hi_db) = (-18.0f64, 12.0f64);
    let width = ui.available_width().min(720.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 150.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let vis = ui.visuals();
    painter.rect_filled(rect, 4.0, vis.extreme_bg_color);
    let x_of = |f: f64| rect.left() + ((f / 20.0).log10() / 3.0) as f32 * rect.width();
    let y_of = |db: f64| rect.top() + ((hi_db - db.clamp(lo_db, hi_db)) / (hi_db - lo_db)) as f32 * rect.height();
    let grid = egui::Stroke::new(1.0_f32, vis.weak_text_color().gamma_multiply(0.35));
    let font = egui::FontId::proportional(10.0);
    for db in [-12.0, -6.0, 0.0, 6.0] {
        let y = y_of(db);
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            if db == 0.0 {
                egui::Stroke::new(1.0_f32, vis.weak_text_color())
            } else {
                grid
            },
        );
        painter.text(
            egui::pos2(rect.left() + 3.0, y - 1.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{db:+} dB"),
            font.clone(),
            vis.weak_text_color(),
        );
    }
    for (f, label) in [
        (50.0, "50"),
        (100.0, "100"),
        (500.0, "500"),
        (1000.0, "1k"),
        (5000.0, "5k"),
        (10000.0, "10k"),
    ] {
        let x = x_of(f);
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], grid);
        painter.text(
            egui::pos2(x + 2.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            font.clone(),
            vis.weak_text_color(),
        );
    }
    let pts: Vec<egui::Pos2> = eqpresets::log_freqs(20.0, 20000.0, 240)
        .map(|f| egui::pos2(x_of(f), y_of(eqpresets::response_db(t, f))))
        .collect();
    painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0_f32, ACCENT)));
    if let Some(p) = resp.hover_pos() {
        let f = 20.0 * 10f64.powf(3.0 * ((p.x - rect.left()) / rect.width()) as f64);
        painter.text(
            p + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{f:.0} Hz  {:+.1} dB", eqpresets::response_db(t, f)),
            egui::FontId::proportional(12.0),
            vis.strong_text_color(),
        );
    }
    ui.label(
        RichText::new("approximate response (standard filter maths; the chip's own design may differ slightly)")
            .weak()
            .small(),
    );
}

fn isd_view(ui: &mut egui::Ui, data: &[u8]) {
    match isdcfg::parse(data) {
        Ok(c) => {
            ui.label(RichText::new("Boot / download settings. Read-only: changing these can stop the chip from booting.").weak());
            egui::Grid::new("isd").num_columns(2).striped(true).show(ui, |ui| {
                ui.label("Chip key");
                ui.label(RichText::new(c.chip_key.map(|k| format!("0x{k:04X}")).unwrap_or_else(|| "invalid".into())).monospace());
                ui.end_row();
                for (k, v) in &c.entries {
                    ui.label(k);
                    ui.label(RichText::new(isdcfg::show_value(v)).monospace());
                    ui.end_row();
                }
            });
        }
        Err(e) => {
            ui.colored_label(BAD, format!("Could not decode: {e:#}"));
        }
    }
}

fn hex_view(ui: &mut egui::Ui, data: &[u8], max_height: f32) {
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
    let rows = data.len().div_ceil(16);
    egui::ScrollArea::vertical()
        .id_salt("hex")
        .max_height(max_height)
        .auto_shrink([false, false])
        .show_rows(ui, row_h, rows, |ui, range| {
            for r in range {
                let chunk = &data[r * 16..(r * 16 + 16).min(data.len())];
                let mut hex = String::with_capacity(52);
                for (i, b) in chunk.iter().enumerate() {
                    if i == 8 {
                        hex.push(' ');
                    }
                    hex.push_str(&format!("{b:02X} "));
                }
                let ascii: String = chunk
                    .iter()
                    .map(|&b| if (0x20..0x7F).contains(&b) { b as char } else { '·' })
                    .collect();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{:06X}", r * 16)).monospace().color(ACCENT));
                    ui.label(RichText::new(format!("{hex:<50}")).monospace());
                    ui.label(RichText::new(ascii).monospace().weak());
                });
            }
        });
}

fn ranges(sectors: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < sectors.len() {
        let start = sectors[i];
        let mut end = start + SECTOR;
        while i + 1 < sectors.len() && sectors[i + 1] == end {
            i += 1;
            end += SECTOR;
        }
        out.push(format!("0x{start:05X}–0x{end:05X}"));
        i += 1;
    }
    out.join(", ")
}

fn save_backup(data: &[u8]) -> std::io::Result<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = home.join("Documents").join("jl-explorer-backups");
    std::fs::create_dir_all(&dir)?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("flash_{secs}.bin"));
    std::fs::write(&path, data)?;
    Ok(path)
}

fn restart_as_admin() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let cmd = format!(
            "Start-Process -FilePath '{}' -Verb RunAs",
            exe.display().to_string().replace('\'', "''")
        );
        if std::process::Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &cmd])
            .spawn()
            .is_ok()
        {
            std::process::exit(0);
        }
    }
}
