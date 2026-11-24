use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::Duration,
};

use anyhow::{Context, Result};
use eframe::egui::{self, Color32, RichText};

use crate::{bundle, config::Settings, launch};

enum Event {
    Status(String),
    Done(Result<()>),
}

struct Launcher {
    root: PathBuf,
    grass_block: egui::TextureHandle,
    settings: Settings,
    message: String,
    error: bool,
    busy: bool,
    events: Option<Receiver<Event>>,
    bundled: bool,
    advanced: bool,
}

impl Launcher {
    fn new(root: PathBuf, grass_block: egui::TextureHandle) -> Self {
        let (settings, message, error) = match Settings::load(&root) {
            Ok(s) => (s, String::new(), false),
            Err(e) => (Settings::default(), format!("{e:#}"), true),
        };
        let bundled = bundle::embedded().is_ok();
        Self {
            root,
            grass_block,
            settings,
            message,
            error,
            busy: false,
            events: None,
            bundled,
            advanced: false,
        }
    }

    fn work(&mut self, repair: bool, ctx: &egui::Context) {
        if let Err(e) = self.settings.save(&self.root) {
            self.message = format!("{e:#}");
            self.error = true;
            return;
        }
        let root = self.root.clone();
        let settings = self.settings.clone();
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.events = Some(rx);
        self.busy = true;
        self.error = false;
        self.message = "Preparing…".into();
        std::thread::spawn(move || {
            let report = |s| {
                let _ = tx.send(Event::Status(s));
                ctx.request_repaint();
            };
            let result = if repair {
                (|| -> Result<()> {
                    let _guard = bundle::lock(&root, "game.lock")?;
                    let (mut exe, payload) = bundle::embedded()?;
                    payload.install(&mut exe, &root, true, &report)?;
                    report("Files repaired.".into());
                    Ok(())
                })()
            } else {
                launch::run(&root, &settings, &report)
            };
            let _ = tx.send(Event::Done(result));
            ctx.request_repaint();
        });
    }

    fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.image((self.grass_block.id(), egui::vec2(32.0, 32.0)));
            ui.heading(RichText::new("Transcraft").size(30.0));
        });
        ui.label(RichText::new("Minecraft 26.3 · Fabric").color(Color32::from_rgb(160, 174, 180)));
        ui.add_space(24.0);
        ui.add_enabled_ui(!self.busy, |ui| {
            egui::Grid::new("settings")
                .spacing([24.0, 14.0])
                .show(ui, |ui| {
                    ui.label("Nickname");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.nickname)
                            .desired_width(240.0)
                            .char_limit(16),
                    )
                    .on_hover_text("Changing nickname changes your player identity and inventory.");
                    ui.end_row();
                    ui.label("Memory");
                    ui.add(
                        egui::Slider::new(&mut self.settings.memory_mib, 1024..=16384)
                            .step_by(256.0)
                            .suffix(" MiB"),
                    );
                    ui.end_row();
                    ui.label("Window size");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.settings.width).range(640..=7680));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut self.settings.height).range(360..=4320));
                        ui.checkbox(&mut self.settings.fullscreen, "Fullscreen");
                    });
                    ui.end_row();
                });
            ui.add_space(12.0);
            ui.checkbox(&mut self.advanced, "More options");
            if self.advanced {
                ui.add_space(8.0);
                ui.label("Game folder");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.game_directory)
                        .desired_width(f32::INFINITY)
                        .hint_text("Default game folder"),
                );
                ui.horizontal(|ui| {
                    if ui.button("Save settings").clicked() {
                        match self.settings.save(&self.root) {
                            Ok(()) => {
                                self.message = "Settings saved".into();
                                self.error = false;
                            }
                            Err(e) => {
                                self.message = format!("{e:#}");
                                self.error = true;
                            }
                        }
                    }
                    if ui
                        .add_enabled(self.bundled, egui::Button::new("Repair files"))
                        .clicked()
                    {
                        self.work(true, ctx);
                    }
                });
                ui.small(format!("Data: {}", self.root.display()));
                ui.small(format!(
                    "Game & logs: {}",
                    self.settings.game_dir(&self.root).display()
                ));
            }
            ui.add_space(16.0);
            let valid = self.settings.validate();
            if let Err(e) = &valid {
                ui.colored_label(Color32::from_rgb(245, 181, 125), e.to_string());
            }
            if ui
                .add_enabled(
                    self.bundled && valid.is_ok(),
                    egui::Button::new(
                        RichText::new("Play")
                            .size(18.0)
                            .color(Color32::from_rgb(15, 32, 23)),
                    )
                    .fill(Color32::from_rgb(125, 208, 151))
                    .min_size(egui::vec2(ui.available_width(), 46.0)),
                )
                .clicked()
            {
                self.work(false, ctx);
            }
        });
        if !self.bundled {
            ui.add_space(12.0);
            ui.colored_label(
                Color32::from_rgb(245, 181, 125),
                "Game bundle missing. See README to build an offline copy.",
            );
        }
        if self.busy || !self.message.is_empty() {
            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                if self.busy {
                    ui.spinner();
                }
                let color = if self.error {
                    Color32::from_rgb(255, 153, 142)
                } else {
                    Color32::from_rgb(164, 180, 185)
                };
                ui.label(RichText::new(&self.message).color(color));
            });
        }
    }
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.events {
            for event in rx.try_iter() {
                match event {
                    Event::Status(s) => {
                        self.message = if s == "Ready to play" {
                            String::new()
                        } else {
                            s
                        };
                    }
                    Event::Done(result) => {
                        self.busy = false;
                        if let Err(e) = result {
                            self.message = format!("{e:#}");
                            self.error = true;
                        }
                    }
                }
            }
        }
        if self.busy && ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.message =
                "Close Minecraft and wait for the current operation before closing the launcher."
                    .into();
        }
        let panel_frame = egui::Frame::new()
            .fill(Color32::from_rgb(20, 25, 28))
            .inner_margin(30.0);

        egui::CentralPanel::default()
            .frame(panel_frame)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.content(ui, ctx));
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

pub fn run(root: PathBuf) -> Result<()> {
    let icon = image::load_from_memory(include_bytes!("../assets/grass-block.png"))
        .context("Cannot decode grass block icon")?
        .to_rgba8();
    let pixels = egui::ColorImage::from_rgba_unmultiplied(
        [icon.width() as usize, icon.height() as usize],
        icon.as_raw(),
    );
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Transcraft")
            .with_icon(egui::IconData {
                width: icon.width(),
                height: icon.height(),
                rgba: icon.into_raw(),
            })
            .with_inner_size([570.0, 440.0])
            .with_min_inner_size([570.0, 440.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Transcraft",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            cc.egui_ctx.style_mut(|s| {
                s.spacing.item_spacing = egui::vec2(10.0, 8.0);
                s.spacing.button_padding = egui::vec2(12.0, 8.0);
            });
            let grass_block =
                cc.egui_ctx
                    .load_texture("grass-block", pixels, egui::TextureOptions::NEAREST);
            Ok(Box::new(Launcher::new(root, grass_block)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("Cannot open launcher window")
}
