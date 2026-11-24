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
    settings: Settings,
    message: String,
    error: bool,
    busy: bool,
    events: Option<Receiver<Event>>,
    bundled: bool,
    advanced: bool,
}

impl Launcher {
    fn new(root: PathBuf) -> Self {
        let (settings, message, error) = match Settings::load(&root) {
            Ok(s) => (s, "Ready to play".into(), false),
            Err(e) => (Settings::default(), format!("{e:#}"), true),
        };
        let bundled = bundle::embedded().is_ok();
        Self {
            root,
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
                    report("Bundled files repaired. Saves and settings are preserved.".into());
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
            ui.label(
                RichText::new("■")
                    .size(32.0)
                    .color(Color32::from_rgb(125, 208, 151)),
            );
            ui.heading(RichText::new("transcraft").size(30.0));
        });
        ui.label(
            RichText::new("Your world. Wherever you are.").color(Color32::from_rgb(160, 174, 180)),
        );
        ui.add_space(20.0);
        egui::Frame::new()
            .fill(Color32::from_rgb(31, 39, 42))
            .inner_margin(18.0)
            .corner_radius(10.0)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(
                    RichText::new("MINECRAFT JAVA  /  26.3")
                        .size(12.0)
                        .color(Color32::from_rgb(125, 208, 151)),
                );
                ui.add_space(7.0);
                ui.label(RichText::new("Fabric · ready for offline play").size(19.0));
                ui.label("Sodium  +  Lithium  +  Mod Menu  +  Fabric API");
            });
        ui.add_space(20.0);
        ui.add_enabled_ui(!self.busy, |ui| {
            egui::Grid::new("settings")
                .spacing([24.0, 14.0])
                .show(ui, |ui| {
                    ui.label("Nickname");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings.nickname)
                            .desired_width(240.0)
                            .char_limit(16),
                    );
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
                ui.label("Game folder (blank uses the default)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.settings.game_directory)
                        .desired_width(f32::INFINITY),
                );
                ui.small("Changing nickname changes your offline player identity and inventory.");
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
                        .add_enabled(self.bundled, egui::Button::new("Repair bundled files"))
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
                        RichText::new("Play Minecraft")
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
        ui.add_space(12.0);
        if !self.bundled {
            ui.colored_label(
                Color32::from_rgb(245, 181, 125),
                concat!(
                    "Launcher only: package the offline bundle with tools/bundle.py ",
                    "before transferring to an offline PC. See README.md.",
                ),
            );
        }
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
        ui.add_space(10.0);
        ui.small(format!(
            "v{}   ·   Local profiles   ·   No sign-in required",
            env!("CARGO_PKG_VERSION")
        ));
    }
}

impl eframe::App for Launcher {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.events {
            for event in rx.try_iter() {
                match event {
                    Event::Status(s) => self.message = s,
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Transcraft")
            .with_inner_size([610.0, 700.0])
            .with_min_inner_size([570.0, 680.0]),
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
            Ok(Box::new(Launcher::new(root)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("Cannot open launcher window")
}
