use eframe::egui;
use notetaker_core::Meeting;
use notetaker_llm::{Cancellation, ChatResult};

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    pub folder: Option<String>,
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
                && crate::theme::button(ui, "К встрече", crate::theme::ButtonStyle::Ghost).clicked()
            {
                action = Some(Action::Open(meeting.clone(), None));
            }
            if ui
                .add_enabled(
                    !self.busy,
                    crate::theme::styled_button(
                        crate::theme::Palette::of(ui),
                        "Очистить диалог",
                        crate::theme::ButtonStyle::Secondary,
                    ),
                )
                .clicked()
            {
                self.messages.clear();
                self.error = None;
            }
            if self.busy
                && crate::theme::button(ui, "Отменить", crate::theme::ButtonStyle::Ghost).clicked()
            {
                self.cancel.cancel();
            }
        });
        action
    }
}

pub fn suggestions(meeting: bool, live: bool) -> &'static [&'static str] {
    if live {
        &[
            "Что я пропустил за последние 5 минут",
            "О чём договорились",
            "Что стоит спросить",
        ]
    } else if meeting {
        &[
            "Итоги и решения",
            "Мои задачи",
            "Что осталось нерешённым",
            "Напиши сообщение участникам",
        ]
    } else {
        &[
            "Что я обещал на этой неделе",
            "Какие решения приняты за неделю",
            "Открытые вопросы",
        ]
    }
}

pub struct Recipe {
    pub name: String,
    pub prompt: Result<String, String>,
}

pub fn recipes(root: &std::path::Path) -> Vec<Recipe> {
    let mut recipes: Vec<_> = [
        ("Итоги для руководителя", "Подготовь итоги для руководителя: результат встречи, решения и вопросы, требующие внимания."),
        ("Задачи с ответственными", "Составь список задач с ответственными и сроками. Если они не названы, укажи это."),
        ("Письмо-follow-up", "Напиши короткое письмо участникам: договорённости, задачи и следующие шаги."),
        ("Риски и блокеры", "Перечисли риски и блокеры, их последствия и согласованные действия."),
        ("Подготовка к следующей встрече", "Подготовь повестку следующей встречи: открытые вопросы и что нужно проверить заранее."),
        ("Выжимка в 5 пунктов", "Сделай краткую выжимку встречи в 5 пунктах."),
    ].into_iter().map(|(name, prompt)| Recipe { name: name.into(), prompt: Ok(prompt.into()) }).collect();
    if let Ok(entries) = std::fs::read_dir(root.join(".recipes")) {
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "md"))
            .collect();
        paths.sort();
        for path in paths {
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let mut name = stem.to_owned();
            let prompt = std::fs::read_to_string(&path)
                .map_err(|e| format!("Не удалось прочитать: {e}"))
                .and_then(|text| {
                    let mut body = Vec::new();
                    let mut heading = false;
                    for line in text.lines() {
                        if !heading && let Some(title) = line.strip_prefix("# ") {
                            if !title.trim().is_empty() {
                                name = title.trim().into();
                            }
                            heading = true;
                        } else {
                            body.push(line);
                        }
                    }
                    let prompt = body.join("\n").trim().to_owned();
                    if prompt.is_empty() {
                        Err("Пустой рецепт".into())
                    } else {
                        Ok(prompt)
                    }
                });
            let recipe = Recipe {
                name: name.clone(),
                prompt,
            };
            if let Some(index) = recipes.iter().position(|r| r.name == name) {
                recipes[index] = recipe;
            } else {
                recipes.push(recipe);
            }
        }
    }
    recipes
}

#[derive(Default)]
pub struct Recipes {
    entries: Vec<Recipe>,
    query: Option<String>,
    selected: usize,
    dismissed: Option<String>,
}

impl Recipes {
    pub fn show(&mut self, ui: &mut egui::Ui, input: &mut String, root: &std::path::Path) -> bool {
        let Some(query) = input.strip_prefix('/') else {
            self.query = None;
            self.dismissed = None;
            return false;
        };
        if self.dismissed.as_deref() == Some(input.as_str()) {
            return false;
        }
        if self.query.is_none() {
            self.entries = recipes(root);
        }
        let mut scroll = self.query.as_deref() != Some(query);
        if scroll {
            self.selected = 0;
        }
        self.query = Some(query.into());
        let matches = filtered(&self.entries, query);
        let mut choose = None;
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.dismissed = Some(input.clone());
            return false;
        }
        if !matches.is_empty() {
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)) {
                self.selected = (self.selected + 1) % matches.len();
                scroll = true;
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)) {
                self.selected = (self.selected + matches.len() - 1) % matches.len();
                scroll = true;
            }
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                choose = Some(matches[self.selected]);
            }
        }
        if let Some(anchor) = ui.ctx().read_response(egui::Id::unique("chat-input")) {
            let mut open = true;
            crate::theme::recipe_popup(ui, &anchor, &mut open, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::ScrollArea::vertical()
                    .id_salt("recipes")
                    .auto_shrink([false, true])
                    .max_height(192.0)
                    .show(ui, |ui| {
                        if matches.is_empty() {
                            ui.weak("Рецепты не найдены");
                        }
                        for (row, index) in matches.iter().copied().enumerate() {
                            let recipe = &self.entries[index];
                            let response = ui
                                .add_enabled_ui(recipe.prompt.is_ok(), |ui| {
                                    crate::theme::recipe_row(
                                        ui,
                                        &recipe.name,
                                        recipe.prompt.as_ref().err().map(String::as_str),
                                        row == self.selected,
                                    )
                                })
                                .inner;
                            if scroll && row == self.selected {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                choose = Some(index);
                            }
                        }
                    });
            });
            if !open {
                self.dismissed = Some(input.clone());
            }
        }
        if let Some(index) = choose
            && let Ok(prompt) = &self.entries[index].prompt
        {
            *input = prompt.clone();
            self.query = None;
            return true;
        }
        false
    }
}

fn filtered(entries: &[Recipe], query: &str) -> Vec<usize> {
    let query = query.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter(|(_, r)| r.name.to_lowercase().contains(&query))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod recipe_tests {
    use super::*;
    #[test]
    fn builtins_override_unreadable_and_filter() {
        let root = tempfile::tempdir().unwrap();
        let builtins = recipes(root.path());
        assert_eq!(builtins.len(), 6);
        std::fs::create_dir(root.path().join(".recipes")).unwrap();
        std::fs::write(
            root.path().join(".recipes/custom.md"),
            "# Итоги для руководителя\nСвоя инструкция",
        )
        .unwrap();
        std::fs::write(root.path().join(".recipes/broken.md"), [0xff]).unwrap();
        let entries = recipes(root.path());
        assert_eq!(entries.len(), 7);
        assert_eq!(entries[0].prompt.as_ref().unwrap(), "Своя инструкция");
        assert!(
            entries
                .iter()
                .find(|r| r.name == "broken")
                .unwrap()
                .prompt
                .is_err()
        );
        assert_eq!(filtered(&entries, "РУКОВОД"), vec![0]);
        assert!(filtered(&entries, "нет такого").is_empty());
        assert_eq!(suggestions(true, false).len(), 4);
        assert_eq!(suggestions(false, false).len(), 3);
        assert!(suggestions(true, true)[0].contains("5 минут"));
    }
}

#[cfg(test)]
mod recipe_ui_tests {
    use super::*;
    #[test]
    fn slash_menu_filters_selects_and_dismisses_with_keyboard() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let root = tempfile::tempdir().unwrap();
        let mut menu = Recipes::default();
        let mut input = "/".to_owned();
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            assert!(!menu.show(ui, &mut input, root.path()));
        })
        .drop_without_applying_deltas();
        crate::test_ui::frame(
            &ctx,
            vec![key(egui::Key::ArrowDown), key(egui::Key::Enter)],
            &mut |ui| {
                assert!(menu.show(ui, &mut input, root.path()));
            },
        )
        .drop_without_applying_deltas();
        assert!(input.starts_with("Составь список задач"));
        input = "/РИСКИ".into();
        crate::test_ui::frame(&ctx, vec![key(egui::Key::Escape)], &mut |ui| {
            assert!(!menu.show(ui, &mut input, root.path()));
        })
        .drop_without_applying_deltas();
        assert_eq!(input, "/РИСКИ");
        assert_eq!(menu.dismissed.as_deref(), Some("/РИСКИ"));
    }
}
