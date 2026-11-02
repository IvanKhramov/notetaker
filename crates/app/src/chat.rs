use eframe::egui;
use notetaker_core::Meeting;
use notetaker_llm::{Cancellation, ChatResult};

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    pub scope: Option<Meeting>,
    pub scope_title: String,
    pub question: String,
    pub pending: String,
    pub messages: Vec<ChatResult>,
    pub streaming: String,
    pub busy: bool,
    pub cancel: Cancellation,
    pub error: Option<String>,
}
pub enum Action {
    Open(Meeting, Option<notetaker_core::Anchor>),
}
impl Panel {
    pub fn open(&mut self, scope: Option<Meeting>, title: String) {
        if self.busy {
            self.open = true;
            return;
        }
        if self.scope.as_ref().map(|m| m.path()) != scope.as_ref().map(|m| m.path()) {
            self.messages.clear();
            self.question.clear();
            self.error = None;
        }
        self.scope = scope;
        self.scope_title = title;
        self.open = true;
    }
    pub fn content(&mut self, ui: &mut egui::Ui, folder: Option<&str>) -> Option<Action> {
        let mut action = None;
        crate::theme::page_heading(ui, "Чат");
        ui.label(if self.scope.is_some() {
            format!("Про встречу: {}", self.scope_title)
        } else if let Some(folder) = folder {
            if folder.is_empty() {
                "По встречам без папки".into()
            } else {
                format!("По папке: {folder}")
            }
        } else {
            "По всем встречам".into()
        });
        if self.messages.is_empty() && !self.busy {
            ui.add_space(24.0);
            ui.weak("Решения, задачи и идеи из ваших встреч. Задайте вопрос ниже.");
        }
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - 60.0).max(80.0))
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for message in &self.messages {
                    ui.strong(&message.turn.question);
                    ui.label(&message.turn.answer);
                    ui.horizontal_wrapped(|ui| {
                        for citation in &message.citations {
                            if ui
                                .link(format!(
                                    "[{}] {}, {}",
                                    citation.number, citation.title, citation.date
                                ))
                                .clicked()
                            {
                                action = Some(Action::Open(
                                    citation.meeting.clone(),
                                    citation.anchor.clone(),
                                ));
                            }
                        }
                    });
                    ui.separator();
                }
                if self.busy {
                    ui.strong(&self.pending);
                    ui.label(&self.streaming);
                    ui.spinner();
                }
            });
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal(|ui| {
            if let Some(meeting) = &self.scope
                && ui.button("К встрече").clicked()
            {
                action = Some(Action::Open(meeting.clone(), None));
            }
            if ui
                .add_enabled(!self.busy, egui::Button::new("Очистить диалог"))
                .clicked()
            {
                self.messages.clear();
                self.error = None;
            }
            if self.busy && ui.button("Отменить").clicked() {
                self.cancel.cancel();
            }
        });
        action
    }
}
