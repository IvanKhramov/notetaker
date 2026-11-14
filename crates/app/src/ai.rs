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
    Save,
    Login,
    Install,
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
    /// The vendor installer is running for this card.
    installing: bool,
    edited: Option<Instant>,
    key_editing: bool,
    save_error: bool,
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
            edited: None,
            key_editing: false,
            save_error: false,
            installed: cli.is_none_or(|kind| notetaker_llm::find_cli(kind).is_some()),
            installing: false,
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
    pub fn has_save_errors(&self) -> bool {
        self.cards.iter().any(|card| card.save_error)
    }
    pub fn has_edits(&self) -> bool {
        self.cards
            .iter()
            .any(|card| card.edited.is_some() || (card.key_editing && !card.key.is_empty()))
    }
    pub fn wake_after(&self, now: Instant) -> Option<Duration> {
        self.cards
            .iter()
            .filter(|card| !card.busy && !card.key_editing && !card.save_error)
            .filter_map(|card| card.edited)
            .map(|due| due.saturating_duration_since(now))
            .min()
    }
    /// Called even when Settings is closed, so a debounce cannot get stranded.
    pub fn autosaves(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        for (index, card) in self.cards.iter_mut().enumerate() {
            if card.busy
                || card.key_editing
                || !card.edited.is_some_and(|due| Instant::now() >= due)
            {
                continue;
            }
            match card.provider.config() {
                Ok(_) => {
                    card.edited = None;
                    card.busy = true;
                    actions.push(Action {
                        index,
                        operation: Operation::Save,
                        provider: card.provider.clone(),
                        key: card.key.clone(),
                    });
                }
                Err(error) => {
                    card.status = format!("Ошибка: {error:#}");
                    card.save_error = true;
                    card.edited = None;
                }
            }
        }
        actions
    }
    pub fn blur(&mut self) {
        for card in &mut self.cards {
            if card.key_editing && !card.key.is_empty() {
                card.edited = Some(Instant::now());
            }
            card.key_editing = false;
        }
    }
    pub fn load(&mut self, settings: &LlmSettings) {
        for card in &mut self.cards {
            if card.busy || card.edited.is_some() || card.key_editing {
                continue;
            }
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
        card.installing = false;
        card.installed = card
            .cli
            .is_none_or(|kind| notetaker_llm::find_cli(kind).is_some());
        if matches!(response.operation, Operation::Save | Operation::Default) {
            card.save_error = response.result.is_err();
        }
        match &response.result {
            Err(error) => card.status = format!("Ошибка: {error}"),
            Ok(models) => {
                if response.operation == Operation::Install {
                    card.status = "Установлено. Теперь войдите в подписку".into();
                    return;
                }
                if response.operation == Operation::Login {
                    card.status = "Не подключено. После входа нажмите Проверить".into();
                    return;
                }
                card.status = if response.operation == Operation::Save {
                    "Сохранено · соединение не проверено".into()
                } else {
                    format!("Подключено · {} мс", response.latency.as_millis())
                };
                if card.edited.is_none() {
                    card.provider = response.provider.clone();
                }
                if !models.is_empty() {
                    card.models = models.clone();
                    if !models.contains(&card.provider.model) {
                        card.provider.model = models[0].clone();
                    }
                }
                if matches!(response.operation, Operation::Default | Operation::Save)
                    && card.edited.is_none()
                {
                    card.key.clear();
                }
            }
        }
    }
    /// Reuse connection actions without exposing scenario/model administration.
    pub fn onboarding_ui(
        &mut self,
        ui: &mut egui::Ui,
        can_default: bool,
        selected: &mut usize,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        crate::theme::select("onboarding_ai")
            .selected_text(self.cards.get(*selected).map_or("Выберите AI", |c| c.title))
            .show_ui(ui, |ui| {
                for (index, card) in self.cards.iter().enumerate() {
                    crate::theme::select_value(ui, selected, index, card.title);
                }
            });
        if let Some(card) = self.cards.get_mut(*selected) {
            let mut operation = None;
            // A missing CLI is explained by the setup block, not by a raw error line.
            if card.cli.is_none() || card.installed {
                status_line(ui, &card.status);
            }
            ui.add_enabled_ui(!card.busy, |ui| {
                if let Some(kind) = card.cli {
                    operation = cli_setup(ui, kind, card.installed, card.installing, &card.status);
                } else {
                    if *selected != 2 {
                        ui.add(
                            crate::theme::text_field(&mut card.provider.base_url)
                                .hint_text("Адрес API"),
                        );
                    }
                    ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                        ui.add(
                            crate::theme::text_field(&mut card.key)
                                .password(true)
                                .hint_text("API-ключ"),
                        );
                    });
                    ui.add(crate::theme::text_field(&mut card.provider.model).hint_text("Модель"));
                }
                if (card.cli.is_none() || card.installed)
                    && ui
                        .add_enabled(
                            can_default,
                            crate::theme::styled_button(
                                crate::theme::Palette::of(ui),
                                "Подключить",
                                crate::theme::ButtonStyle::Secondary,
                            ),
                        )
                        .clicked()
                {
                    operation = Some(Operation::Default);
                }
            });
            if card.busy {
                ui.spinner();
            }
            if let Some(operation) = operation {
                card.busy = true;
                card.installing = operation == Operation::Install;
                card.edited = None;
                actions.push(Action {
                    index: *selected,
                    operation,
                    provider: card.provider.clone(),
                    key: card.key.clone(),
                });
            }
        }
        actions
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
            let usable = card.cli.is_none() || card.installed;
            ui.push_id(index, |ui| { crate::theme::settings_group(ui,"",|ui| {crate::theme::disclosure(card.title).status(status_chip(&card.status, usable, default == card.provider.id))
                .default_open(default == card.provider.id || selected.is_some()).show(ui, |ui| {
                if usable { status_line(ui, &card.status); }
                let before = serde_json::to_value(&card.provider).expect("provider serialize");
                let mut operation = None;
                let mut typing = false;
                let mut blur = false;
                let mut key_blur = false;
                ui.add_enabled_ui(!card.busy, |ui| {
                    if let Some(kind) = card.cli {
                        if let Some(requested) = cli_setup(ui, kind, card.installed, card.installing, &card.status) { operation = Some(requested); }
                        if card.installed && kind == ProviderKind::ClaudeCli {
                            crate::theme::form_field(ui, "Модель", None, |ui| {
                                crate::theme::select("model").selected_text(if card.provider.model.is_empty() { "Авто" } else { &card.provider.model }).show_ui(ui, |ui| {
                                    for (value, label) in [("", "Авто"), ("sonnet", "Sonnet"), ("opus", "Opus"), ("haiku", "Haiku")] {
                                        crate::theme::select_value(ui, &mut card.provider.model, value.into(), label);
                                    }
                                });
                            });
                        }
                        note(ui, "Пользовательские MCP отключены. Ограничение инструментов CLI не гарантирует изоляцию локальных файлов: доступ зависит от версии CLI и managed-настроек. Для строгой конфиденциальности используйте HTTP-провайдер.");
                    } else {
                        if index == 2 {
                            ui.small("Ключ: 4o.flant.ru → Settings → Account → API Keys. Шлюз доступен из VPN Фланта.");
                        } else {
                            crate::theme::setting_row(ui, "Адрес API", |ui| {
                                let response = ui.add(crate::theme::text_field(&mut card.provider.base_url));
                                typing |= response.changed(); blur |= response.lost_focus();
                            });
                            crate::theme::select("kind").selected_text(if card.provider.kind == "anthropic" { "Anthropic" } else { "OpenAI-совместимый" }).show_ui(ui, |ui| {
                                crate::theme::select_value(ui, &mut card.provider.kind, "openai".into(), "OpenAI-совместимый");
                                crate::theme::select_value(ui, &mut card.provider.kind, "anthropic".into(), "Anthropic");
                            });
                        }
                        ui.label("API-ключ (пусто - использовать сохранённый)");
                        ui.add_enabled_ui(cfg!(any(windows, target_os = "macos")), |ui| {
                            let response = ui.add(crate::theme::text_field(&mut card.key).password(true));
                            card.key_editing = response.has_focus();
                            key_blur = response.lost_focus() && !card.key.is_empty();
                        });
                        #[cfg(not(any(windows, target_os = "macos")))]
                        ui.small("На этой ОС задайте api_key_env в Расширенных и переменную окружения перед запуском.");
                        if index == 3 { crate::theme::setting_row(ui, "Модель", |ui| {
                            let response = ui.add(crate::theme::text_field(&mut card.provider.model));
                            typing |= response.changed(); blur |= response.lost_focus();
                        }); }
                        if !card.models.is_empty() {
                            crate::theme::select("model").selected_text(&card.provider.model).show_ui(ui, |ui| {
                                for model in &card.models { crate::theme::select_value(ui, &mut card.provider.model, model.clone(), model); }
                            });
                        }
                        if selected.is_none() && crate::theme::button(ui, "Загрузить список моделей", crate::theme::ButtonStyle::Ghost).clicked() { operation = Some(Operation::Models); }
                    }
                    if usable { ui.add_space(4.0); ui.horizontal(|ui| {
                        if ui.add(crate::theme::styled_button(crate::theme::Palette::of(ui), "Проверить", crate::theme::ButtonStyle::Secondary).icon(crate::theme::icons::REFRESH_CW)).clicked() { operation = Some(Operation::Check); }
                        if default != card.provider.id && ui.add_enabled(can_default, crate::theme::styled_button(crate::theme::Palette::of(ui),if selected.is_some() { "Подключить" } else { "Использовать по умолчанию" },crate::theme::ButtonStyle::Secondary)).clicked() { operation = Some(Operation::Default); }
                    }); }
                });
                if before != serde_json::to_value(&card.provider).expect("provider serialize") {
                    card.save_error = false;
                    card.edited = Some(Instant::now() + if typing { Duration::from_millis(700) } else { Duration::ZERO });
                }
                if key_blur || (blur && card.edited.is_some()) { card.edited = Some(Instant::now()); }
                if card.busy { ui.spinner(); }
                if let Some(operation) = operation {
                    card.busy = true;
                    card.installing = operation == Operation::Install;
                    if matches!(operation, Operation::Save | Operation::Default) { card.edited = None; }
                    actions.push(Action { index, operation, provider: card.provider.clone(), key: if card.key_editing { String::new() } else { card.key.clone() } });
                }
            }); }); });
        }
        actions
    }
}

/// Short state for the card header; the full text is shown inside the card.
fn status_chip(status: &str, usable: bool, default: bool) -> &str {
    if !usable {
        "Не установлен"
    } else if status.starts_with("Ошибка") {
        "Ошибка"
    } else if status.starts_with("Подключено") {
        if default {
            "По умолчанию"
        } else {
            "Подключено"
        }
    } else if status.starts_with("Сохранено") || status.starts_with("Настроено") {
        "Не проверено"
    } else {
        "Не подключено"
    }
}
fn status_line(ui: &mut egui::Ui, status: &str) {
    let p = crate::theme::Palette::of(ui);
    if let Some(error) = status.strip_prefix("Ошибка: ") {
        crate::theme::callout(ui, crate::theme::icons::CIRCLE_ALERT, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(error).color(p.red)).wrap());
        });
    } else {
        ui.add(egui::Label::new(egui::RichText::new(status).size(13.0).color(p.muted)).wrap());
    }
}
fn note(ui: &mut egui::Ui, text: &str) {
    let p = crate::theme::Palette::of(ui);
    ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(p.muted)).wrap());
}
/// What a subscription needs before it can be used: its CLI, then a login.
fn cli_setup(
    ui: &mut egui::Ui,
    kind: ProviderKind,
    installed: bool,
    installing: bool,
    status: &str,
) -> Option<Operation> {
    use crate::theme::{ButtonStyle, Palette, icons};
    let claude = kind == ProviderKind::ClaudeCli;
    let p = Palette::of(ui);
    let mut operation = None;
    if !installed {
        let (name, vendor, url) = if claude {
            (
                "Claude Code",
                "Anthropic",
                "https://code.claude.com/docs/en/setup",
            )
        } else {
            (
                "Codex CLI",
                "OpenAI",
                "https://developers.openai.com/codex/cli",
            )
        };
        crate::theme::callout(ui, icons::INFO, |ui| {
            ui.add(
                egui::Label::new(format!(
                    "Вход по подписке работает через {name}. На этом компьютере он не найден."
                ))
                .wrap(),
            );
            if installing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Устанавливаю {name}. Это займёт пару минут."));
                });
                return;
            }
            // The card status is hidden while the CLI is missing, so a failed install shows here.
            if let Some(error) = status.strip_prefix("Ошибка: ")
                && !error.starts_with("CLI не установлен")
            {
                ui.add(egui::Label::new(egui::RichText::new(error).color(p.red)).wrap());
            }
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(
                        crate::theme::styled_button(
                            p,
                            &format!("Установить {name}"),
                            ButtonStyle::Primary,
                        )
                        .icon(icons::DOWNLOAD),
                    )
                    .clicked()
                {
                    operation = Some(Operation::Install);
                }
                if crate::theme::button(ui, "Проверить снова", ButtonStyle::Ghost).clicked()
                {
                    operation = Some(Operation::Check);
                }
                if ui
                    .add(
                        crate::theme::styled_button(p, "Инструкция", ButtonStyle::Ghost)
                            .icon(icons::EXTERNAL_LINK),
                    )
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            });
            note(
                ui,
                &format!(
                    "Запустится официальный установщик {vendor}. Он ставит программу в профиль пользователя, права администратора не нужны."
                ),
            );
        });
    } else {
        note(
            ui,
            if claude {
                "Откроется окно Claude Code. Выполните в нём /login, затем вернитесь и нажмите «Проверить»."
            } else {
                "Откроется браузер для входа в ChatGPT. После входа нажмите «Проверить»."
            },
        );
        if ui
            .add(
                crate::theme::styled_button(p, "Войти в подписку", ButtonStyle::Secondary)
                    .icon(icons::EXTERNAL_LINK),
            )
            .clicked()
        {
            operation = Some(Operation::Login);
        }
    }
    operation
}

pub fn execute(mut action: Action) -> Response {
    let started = Instant::now();
    let result = (|| -> Result<Vec<String>> {
        let mut draft = action.provider.clone();
        if action.operation == Operation::Models && draft.model.is_empty() {
            draft.model = "models".into();
        }
        let config = draft.config()?;
        if action.operation == Operation::Save {
            if !action.key.is_empty() {
                OsCredentialStore.write(&action.provider.id, &action.key)?;
                action.provider.key_source = "credential".into();
                action.provider.api_key_env = None;
            }
            return Ok(Vec::new());
        }
        if action.operation == Operation::Install {
            notetaker_llm::install_cli(config.kind)?;
            return Ok(Vec::new());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_draft_waits_for_blur_even_after_debounce() {
        let mut connections = Connections::default();
        let card = &mut connections.cards[3];
        card.provider.base_url = "http://localhost:11434/v1".into();
        card.provider.model = "qwen3:8b".into();
        card.key = "<TOKEN>".into();
        card.key_editing = true;
        card.edited = Some(Instant::now() - Duration::from_secs(1));
        assert!(connections.autosaves().is_empty());
        connections.blur();
        let actions = connections.autosaves();
        assert_eq!(actions.len(), 1);
        assert!(actions[0].operation == Operation::Save);
        assert_eq!(actions[0].key, "<TOKEN>");
        assert!(connections.autosaves().is_empty());
    }

    #[test]
    fn incomplete_connection_does_not_reach_the_worker() {
        let mut connections = Connections::default();
        connections.cards[3].edited = Some(Instant::now());
        assert!(connections.autosaves().is_empty());
        assert!(connections.has_save_errors());
        assert!(!connections.cards[3].busy);
    }
}
