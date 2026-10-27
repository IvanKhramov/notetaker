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
    Ask,
    Open(Meeting),
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
    pub fn show(&mut self, ctx: &egui::Context, can_ask: bool) -> Option<Action> {
        let mut action = None;
        egui::Window::new("Спросить")
            .open(&mut self.open)
            .default_size([620.0, 600.0])
            .show(ctx, |ui| {
                ui.label(if self.scope.is_some() {
                    format!("Про встречу: {}", self.scope_title)
                } else {
                    "По всем встречам".into()
                });
                ui.small("История только в памяти. Local-данные направляются локальной модели.");
                egui::ScrollArea::vertical()
                    .max_height(420.0)
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
                                        action = Some(Action::Open(citation.meeting.clone()));
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
                    ui.colored_label(egui::Color32::RED, error);
                }
                ui.add_enabled(
                    !self.busy,
                    egui::TextEdit::multiline(&mut self.question)
                        .desired_rows(3)
                        .hint_text("Что решили про ...?"),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            can_ask && !self.busy && !self.question.trim().is_empty(),
                            egui::Button::new("Спросить"),
                        )
                        .clicked()
                    {
                        action = Some(Action::Ask);
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
            });
        action
    }
}
