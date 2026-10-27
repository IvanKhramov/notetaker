use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui;
use notetaker_core::Mode;
use notetaker_llm::{
    Cancellation, CliProvider, CompletionRequest, CredentialStore, HttpProvider, OsCredentialStore,
    Provider, ProviderKind,
};

use crate::config::{LlmSettings, ProviderSettings};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Check,
    Models,
    Default,
    Login,
}
pub struct Action {
    pub index: usize,
    pub operation: Operation,
    pub provider: ProviderSettings,
    pub key: String,
}
pub struct Response {
    pub index: usize,
    pub operation: Operation,
    pub provider: ProviderSettings,
    pub result: Result<Vec<String>, String>,
    pub latency: Duration,
}
struct Card {
    title: &'static str,
    provider: ProviderSettings,
    cli: Option<ProviderKind>,
    key: String,
    models: Vec<String>,
    status: String,
    busy: bool,
    installed: bool,
}
pub struct Connections {
    cards: Vec<Card>,
}
impl Default for Connections {
    fn default() -> Self {
        let cards = [
            (
                "Claude (подписка)",
                "claude-subscription",
                "claude-cli",
                "",
                "",
                Some(ProviderKind::ClaudeCli),
            ),
            (
                "ChatGPT (подписка)",
                "codex-subscription",
                "codex-cli",
                "",
                "",
                Some(ProviderKind::CodexCli),
            ),
            (
                "Flant LLM Gateway",
                "flant",
                "openai",
                "https://llm-api.flant.ru/v1",
                "claude-sonnet-4-6",
                None,
            ),
            (
                "Свой шлюз",
                "custom-gateway",
                "openai",
                "https://",
                "",
                None,
            ),
        ]
        .into_iter()
        .map(|(title, id, kind, base_url, model, cli)| Card {
            title,
            cli,
            installed: cli.is_none_or(|kind| notetaker_llm::find_cli(kind).is_some()),
            key: String::new(),
            models: Vec::new(),
            status: "Не подключено".into(),
            busy: false,
            provider: ProviderSettings {
                id: id.into(),
                kind: kind.into(),
                base_url: base_url.into(),
                model: model.into(),
                context_tokens: if cli.is_some() { 150_000 } else { 32_768 },
                ..ProviderSettings::default()
            },
        })
        .collect();
        Self { cards }
    }
}
impl Connections {
    pub fn load(&mut self, settings: &LlmSettings) {
        for card in &mut self.cards {
            if let Some(provider) = settings.providers.iter().find(|p| p.id == card.provider.id) {
                card.provider = provider.clone();
                if card.status == "Не подключено" {
                    card.status = "Настроено · нажмите Проверить".into();
                }
            }
        }
    }
    pub fn apply(&mut self, response: &Response) {
        let card = &mut self.cards[response.index];
        card.busy = false;
        card.installed = card
            .cli
            .is_none_or(|kind| notetaker_llm::find_cli(kind).is_some());
        match &response.result {
            Err(error) => card.status = format!("Ошибка: {error}"),
            Ok(models) => {
                if response.operation == Operation::Login {
                    card.status = "Не подключено. После входа нажмите Проверить".into();
                    return;
                }
                card.status = format!("Подключено · {} мс", response.latency.as_millis());
                card.provider = response.provider.clone();
                if !models.is_empty() {
                    card.models = models.clone();
                    if !models.contains(&card.provider.model) {
                        card.provider.model = models[0].clone();
                    }
                }
                if response.operation == Operation::Default {
                    card.key.clear();
                }
            }
        }
    }
    pub fn provider_title<'a>(&'a self, id: &'a str) -> &'a str {
        self.cards
            .iter()
            .find(|card| card.provider.id == id)
            .map_or(id, |card| card.title)
    }
    pub fn provider_index(&self, id: &str) -> Option<usize> {
        self.cards.iter().position(|card| card.provider.id == id)
    }
    pub fn provider_error(&self, id: &str) -> Option<&str> {
        self.cards
            .iter()
            .find(|card| card.provider.id == id)
            .and_then(|card| {
                card.status
                    .starts_with("Ошибка:")
                    .then_some(card.status.as_str())
            })
    }
    pub fn quickstart_ui(
        &mut self,
        ui: &mut egui::Ui,
        default: &str,
        can_default: bool,
        selected: &mut usize,
    ) -> Vec<Action> {
        ui.horizontal_wrapped(|ui| {
            for (index, card) in self.cards.iter().enumerate() {
                ui.selectable_value(selected, index, card.title);
            }
        });
        self.cards_ui(ui, default, can_default, Some(*selected))
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, default: &str, can_default: bool) -> Vec<Action> {
        self.cards_ui(ui, default, can_default, None)
    }
    fn cards_ui(
        &mut self,
        ui: &mut egui::Ui,
        default: &str,
        can_default: bool,
        selected: Option<usize>,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        for (index, card) in self.cards.iter_mut().enumerate() {
            if selected.is_some_and(|selected| selected != index) {
                continue;
            }
            ui.push_id(index, |ui| { ui.group(|ui| {
                ui.heading(card.title);
                if card.status.starts_with("Ошибка:") {
                    ui.colored_label(crate::theme::Palette::of(ui).red, &card.status);
                } else { ui.label(&card.status); }
                let mut operation = None;
                ui.add_enabled_ui(!card.busy, |ui| {
                    if let Some(kind) = card.cli {
                        if !card.installed {
                            ui.label("CLI не установлен");
                            if kind == ProviderKind::ClaudeCli {
                                ui.hyperlink_to("Инструкция по установке", "https://code.claude.com/docs");
                                ui.code("npm i -g @anthropic-ai/claude-code");
                            } else {
                                ui.hyperlink_to("Инструкция по установке", "https://developers.openai.com/codex/cli");
                                ui.code("npm i -g @openai/codex");
                            }
                        }
                        if kind == ProviderKind::ClaudeCli {
                            egui::ComboBox::from_id_salt("model").selected_text(if card.provider.model.is_empty() { "Авто" } else { &card.provider.model }).show_ui(ui, |ui| {
                                for (value, label) in [("", "Авто"), ("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")] {
                                    ui.selectable_value(&mut card.provider.model, value.into(), label);
                                }
                            });
                            ui.small("В окне Claude выполните /login. Токенами входа управляет Claude Code.");
                        } else { ui.small("Вход откроет браузер. Учётными данными управляет Codex CLI."); }
                        ui.small("Пользовательские MCP отключены. Ограничение инструментов CLI не гарантирует изоляцию Local-файлов: доступ зависит от версии CLI и managed-настроек. Для строгой конфиденциальности используйте HTTP-провайдер.");
                        if ui.button("Войти").clicked() { operation = Some(Operation::Login); }
                    } else {
                        if index == 2 {
                            ui.small("Ключ: 4o.flant.ru → Settings → Account → API Keys. Шлюз доступен из VPN Фланта.");
                        } else {
                            ui.label("Адрес API"); ui.text_edit_singleline(&mut card.provider.base_url);
                            egui::ComboBox::from_id_salt("kind").selected_text(if card.provider.kind == "anthropic" { "Anthropic" } else { "OpenAI-совместимый" }).show_ui(ui, |ui| {
                                ui.selectable_value(&mut card.provider.kind, "openai".into(), "OpenAI-совместимый");
                                ui.selectable_value(&mut card.provider.kind, "anthropic".into(), "Anthropic");
                            });
                        }
                        ui.label("API-ключ (пусто - использовать сохранённый)");
                        ui.add(egui::TextEdit::singleline(&mut card.key).password(true));
                        #[cfg(not(windows))]
                        ui.small("На этой ОС задайте api_key_env в Расширенных и переменную окружения перед запуском.");
                        if index == 3 { ui.label("Модель"); ui.text_edit_singleline(&mut card.provider.model); }
                        if !card.models.is_empty() {
                            egui::ComboBox::from_id_salt("model").selected_text(&card.provider.model).show_ui(ui, |ui| {
                                for model in &card.models { ui.selectable_value(&mut card.provider.model, model.clone(), model); }
                            });
                        }
                        if selected.is_none() && ui.button("Загрузить список моделей").clicked() { operation = Some(Operation::Models); }
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Проверить").clicked() { operation = Some(Operation::Check); }
                        if ui.add_enabled(can_default, egui::Button::new(if selected.is_some() { "Подключить" } else { "Использовать по умолчанию" })).clicked() { operation = Some(Operation::Default); }
                        if default == card.provider.id { ui.small("По умолчанию"); }
                    });
                });
                if card.busy { ui.spinner(); }
                if let Some(operation) = operation {
                    card.busy = true;
                    actions.push(Action { index, operation, provider: card.provider.clone(), key: card.key.clone() });
                }
            }); });
        }
        actions
    }
}

pub fn execute(mut action: Action) -> Response {
    let started = Instant::now();
    let result = (|| -> Result<Vec<String>> {
        let mut draft = action.provider.clone();
        if action.operation == Operation::Models && draft.model.is_empty() {
            draft.model = "models".into();
        }
        let config = draft.config()?;
        if action.operation == Operation::Login {
            notetaker_llm::open_login(config.kind)?;
            return Ok(Vec::new());
        }
        if config.kind.is_cli() {
            let provider = CliProvider::new(config, Mode::Hybrid, Cancellation::default())?;
            provider.check_login()?;
            // Codex login status is local, so also verify model access with a tiny request.
            if matches!(action.operation, Operation::Check | Operation::Default)
                && action.provider.kind == "codex-cli"
            {
                provider.check_completion()?;
            }
            return Ok(Vec::new());
        }
        let key = if action.key.is_empty() {
            notetaker_llm::resolve_key(&config, &OsCredentialStore)?
        } else {
            Some(action.key.clone())
        };
        let provider = HttpProvider::with_key(config, Mode::Hybrid, key.clone())?;
        let models = if action.operation == Operation::Models
            || (action.index == 2 && action.operation == Operation::Default)
        {
            let models = provider.models()?;
            if action.index == 2 && !models.contains(&action.provider.model) {
                action.provider.model = if models.iter().any(|id| id == "claude-sonnet-4-6") {
                    "claude-sonnet-4-6".into()
                } else {
                    models[0].clone()
                };
            }
            models
        } else {
            Vec::new()
        };
        if matches!(action.operation, Operation::Check | Operation::Default) {
            let provider = HttpProvider::with_key(action.provider.config()?, Mode::Hybrid, key)?;
            provider.complete(
                &CompletionRequest {
                    system: "Reply briefly.".into(),
                    user: "ok".into(),
                    max_output_tokens: 16,
                },
                &mut |_| {},
            )?;
        }
        if action.operation == Operation::Default && !action.key.is_empty() {
            OsCredentialStore.write(&action.provider.id, &action.key)?;
            action.provider.key_source = "credential".into();
            action.provider.api_key_env = None;
        }
        Ok(models)
    })()
    .map_err(|error| format!("{error:#}"));
    Response {
        index: action.index,
        operation: action.operation,
        provider: action.provider,
        result,
        latency: started.elapsed(),
    }
}
