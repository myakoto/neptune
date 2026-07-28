//! Окно Neptune: субтитры сверху, push-to-talk снизу, статус-строка.

use std::collections::VecDeque;

use eframe::egui::{
    self, Align, Button, CentralPanel, Color32, Key, Layout, Panel, RichText, ScrollArea,
    ViewportCommand,
};
use tokio::sync::mpsc;

use crate::gui::messages::{StatusSnapshot, UiCommand, UiEvent, estimate_cost_usd};

const MAX_SUBTITLES: usize = 200;
const MAX_HISTORY: usize = 20;

/// Одна реплика собеседника.
struct SubtitleLine {
    speaker: Option<u32>,
    text: String,
    translation: Option<String>,
}

/// Один результат push-to-talk.
struct PttEntry {
    recognized: String,
    translated: Option<String>,
}

/// Состояние автообновления.
enum UpdateState {
    /// Обновлений нет (или ещё не проверяли).
    Idle,
    /// Найдена новая версия.
    Available(String),
    /// Идёт скачивание и установка.
    Downloading,
    /// Установлено, ждём перезапуска.
    Ready(String),
}

/// Состояние окна.
pub struct NeptuneApp {
    commands: mpsc::UnboundedSender<UiCommand>,
    events: mpsc::UnboundedReceiver<UiEvent>,
    listening: bool,
    pinned: bool,
    interim: String,
    subtitles: VecDeque<SubtitleLine>,
    ptt_held: bool,
    ptt_interim: String,
    history: VecDeque<PttEntry>,
    status: StatusSnapshot,
    last_error: Option<String>,
    update: UpdateState,
}

impl NeptuneApp {
    /// Создаёт окно и сразу включает субтитры.
    #[must_use]
    pub fn new(
        commands: mpsc::UnboundedSender<UiCommand>,
        events: mpsc::UnboundedReceiver<UiEvent>,
    ) -> Self {
        let _ = commands.send(UiCommand::SetListening(true));
        Self {
            commands,
            events,
            listening: false,
            pinned: true,
            interim: String::new(),
            subtitles: VecDeque::new(),
            ptt_held: false,
            ptt_interim: String::new(),
            history: VecDeque::new(),
            status: StatusSnapshot::default(),
            last_error: None,
            update: UpdateState::Idle,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                UiEvent::SubtitleInterim(text) => self.interim = text,
                UiEvent::SubtitleFinal {
                    speaker,
                    text,
                    translation,
                } => {
                    self.interim.clear();
                    self.last_error = None;
                    self.subtitles.push_back(SubtitleLine {
                        speaker,
                        text,
                        translation,
                    });
                    while self.subtitles.len() > MAX_SUBTITLES {
                        self.subtitles.pop_front();
                    }
                }
                UiEvent::Listening(on) => self.listening = on,
                UiEvent::PttInterim(text) => self.ptt_interim = text,
                UiEvent::PttDone {
                    recognized,
                    translated,
                    ..
                } => {
                    self.ptt_interim.clear();
                    if !recognized.is_empty() {
                        self.history.push_front(PttEntry {
                            recognized,
                            translated,
                        });
                        while self.history.len() > MAX_HISTORY {
                            self.history.pop_back();
                        }
                    }
                }
                UiEvent::Status(snapshot) => self.status = snapshot,
                UiEvent::Error(message) => self.last_error = Some(message),
                UiEvent::UpdateAvailable(version) => {
                    self.update = UpdateState::Available(version);
                }
                UiEvent::UpdateApplied(version) => self.update = UpdateState::Ready(version),
                UiEvent::UpdateFailed(message) => {
                    self.update = UpdateState::Idle;
                    self.last_error = Some(format!("обновление: {message}"));
                }
            }
        }
    }

    /// Баннер обновления; рисуется только когда есть что показать.
    fn update_banner(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        let mut start_download = false;
        match &self.update {
            UpdateState::Idle => {}
            UpdateState::Available(version) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Доступна версия {version}")).small());
                    start_download = ui.small_button("⬇ обновить").clicked();
                });
            }
            UpdateState::Downloading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Скачиваю обновление…").small());
                });
            }
            UpdateState::Ready(version) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Версия {version} установлена")).small());
                    if ui.small_button("🔄 перезапустить").clicked() {
                        restart_app(ctx);
                    }
                });
            }
        }
        if start_download {
            self.send(UiCommand::ApplyUpdate);
            self.update = UpdateState::Downloading;
        }
    }

    fn send(&self, command: UiCommand) {
        let _ = self.commands.send(command);
    }

    fn header(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Neptune").strong());
            ui.label(RichText::new("EN ⇄ RU").weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .selectable_label(self.pinned, "📌")
                    .on_hover_text("Поверх всех окон")
                    .clicked()
                {
                    self.pinned = !self.pinned;
                    let level = if self.pinned {
                        egui::WindowLevel::AlwaysOnTop
                    } else {
                        egui::WindowLevel::Normal
                    };
                    ctx.send_viewport_cmd(ViewportCommand::WindowLevel(level));
                }
                let toggle_text = if self.listening {
                    "⏸ пауза"
                } else {
                    "▶ слушать"
                };
                if ui.button(toggle_text).clicked() {
                    self.send(UiCommand::SetListening(!self.listening));
                }
                let badge = if self.listening {
                    "слушаю"
                } else {
                    "пауза"
                };
                ui.label(RichText::new(badge).weak().small());
            });
        });
    }

    fn subtitles_panel(&self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("🎧 Мне говорят · перевод на русский")
                .weak()
                .small(),
        );
        ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.subtitles {
                    ui.horizontal_wrapped(|ui| {
                        if let Some(speaker) = line.speaker {
                            ui.label(
                                RichText::new(format!("S{speaker}"))
                                    .color(Color32::from_rgb(0x37, 0x8A, 0xDD))
                                    .strong(),
                            );
                        }
                        match &line.translation {
                            Some(translation) => ui.label(translation),
                            None => ui.label(&line.text),
                        };
                    });
                    if line.translation.is_some() {
                        ui.label(RichText::new(&line.text).weak().small());
                    }
                    ui.add_space(4.0);
                }
                if !self.interim.is_empty() {
                    ui.label(
                        RichText::new(format!("… {}", self.interim))
                            .weak()
                            .italics(),
                    );
                }
            });
    }

    fn ptt_panel(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("🎤 Я говорю · перевод на английский")
                .weak()
                .small(),
        );

        let label = if self.ptt_held {
            "● Говорю… (отпусти, чтобы перевести)"
        } else {
            "Говорить — зажми кнопку или Пробел"
        };
        let button = ui.add_sized([ui.available_width(), 36.0], Button::new(label));
        let space_down = ctx.input(|i| i.key_down(Key::Space)) && !ctx.egui_wants_keyboard_input();
        let held_now = button.is_pointer_button_down_on() || space_down;
        if held_now != self.ptt_held {
            self.ptt_held = held_now;
            self.send(if held_now {
                UiCommand::PttPress
            } else {
                UiCommand::PttRelease
            });
        }
        if self.ptt_held {
            ctx.request_repaint();
        }

        if !self.ptt_interim.is_empty() {
            ui.label(
                RichText::new(format!("… {}", self.ptt_interim))
                    .weak()
                    .italics(),
            );
        }

        if let Some(entry) = self.history.front() {
            ui.add_space(4.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(translated) = &entry.translated {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(translated).strong());
                    });
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&entry.recognized).weak().small());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("📋 копировать").clicked() {
                                copy_to_clipboard(translated);
                            }
                        });
                    });
                    ui.label(
                        RichText::new("скопировано в буфер — вставь через Ctrl+V")
                            .weak()
                            .small(),
                    );
                } else {
                    ui.label(&entry.recognized);
                    ui.label(
                        RichText::new("перевода нет — проверь YANDEX_API_KEY")
                            .weak()
                            .small(),
                    );
                }
            });
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            status_dot(ui, "Deepgram", self.status.deepgram_ok);
            match self.status.yandex_ok {
                Some(ok) => status_dot(ui, "Yandex", ok),
                None => {
                    ui.label(RichText::new("○ Yandex: нет ключа").weak().small());
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let cost = estimate_cost_usd(&self.status);
                ui.label(
                    RichText::new(format!(
                        "{:.1} мин · {} симв · ≈${cost:.2}",
                        self.status.audio_seconds / 60.0,
                        self.status.translated_chars
                    ))
                    .weak()
                    .small()
                    .monospace(),
                );
            });
        });
        if let Some(error) = &self.last_error {
            ui.label(
                RichText::new(error)
                    .color(Color32::from_rgb(0xA3, 0x2D, 0x2D))
                    .small(),
            );
        }
    }
}

fn status_dot(ui: &mut egui::Ui, name: &str, ok: bool) {
    let (dot, color) = if ok {
        ("●", Color32::from_rgb(0x1D, 0x9E, 0x75))
    } else {
        ("●", Color32::from_rgb(0xE2, 0x4B, 0x4A))
    };
    ui.label(RichText::new(format!("{dot} {name}")).color(color).small());
}

fn copy_to_clipboard(text: &str) {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(text.to_owned());
    }
}

/// Запускает свежий exe (по тому же пути) и закрывает текущее окно.
fn restart_app(ctx: &egui::Context) {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    ctx.send_viewport_cmd(ViewportCommand::Close);
}

impl eframe::App for NeptuneApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_events();
        let ctx = ui.ctx().clone();

        Panel::top("header").show(ui, |ui| {
            self.header(&ctx, ui);
        });
        if !matches!(self.update, UpdateState::Idle) {
            Panel::top("update").show(ui, |ui| {
                self.update_banner(&ctx, ui);
            });
        }
        Panel::bottom("status").show(ui, |ui| {
            self.status_bar(ui);
        });
        Panel::bottom("ptt").show(ui, |ui| {
            self.ptt_panel(&ctx, ui);
            ui.add_space(4.0);
        });
        CentralPanel::default_margins().show(ui, |ui| {
            self.subtitles_panel(ui);
        });
    }
}
