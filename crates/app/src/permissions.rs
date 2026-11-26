//! Permission facts and capture failures are separate: silence is not a TCC denial.
use eframe::egui;
use notetaker_capture::Permission;

pub const MICROPHONE_DENIED: &str = "Нет доступа к микрофону";

pub struct Access {
    pub query: fn() -> [Permission; 2],
    pub channels: [Permission; 2],
    pub failed: [bool; 2],
    pub check_requested: bool,
    pub show_requested: bool,
    pub repeat_after_microphone_prompt: bool,
    checked: bool,
    focused: bool,
}
impl Default for Access {
    fn default() -> Self {
        Self {
            query: notetaker_capture::permissions,
            channels: [Permission::Unknown; 2],
            failed: [false; 2],
            check_requested: false,
            show_requested: false,
            repeat_after_microphone_prompt: false,
            checked: false,
            focused: false,
        }
    }
}
impl Access {
    pub fn refresh(&mut self) {
        #[cfg(any(target_os = "macos", windows))]
        self.apply_query((self.query)());
    }
    pub fn apply_query(&mut self, channels: [Permission; 2]) {
        if self.repeat_after_microphone_prompt && channels[0] == Permission::Granted {
            self.check_requested = true;
            self.repeat_after_microphone_prompt = false;
        }
        if channels[0] != Permission::Unknown || self.channels[0] != Permission::Denied {
            self.channels[0] = channels[0];
        }
        // System audio has no public preflight query. Preserve a definite failure
        // until a sound check or actual capture demonstrates that audio flows.
        if channels[1] != Permission::Unknown || self.channels[1] != Permission::Denied {
            self.channels[1] = channels[1];
        }
        if channels[0] == Permission::Granted {
            self.failed[0] = false;
        }
        self.checked = true;
    }
    pub fn on_focus(&mut self, focused: bool, query: impl FnOnce() -> [Permission; 2]) {
        if !self.checked || focused && !self.focused {
            self.apply_query(query());
        }
        self.focused = focused;
    }
    pub fn capture_failed(&mut self, index: usize) {
        self.refresh();
        self.failed[index] = true;
        if index == 1 && self.channels[1] != Permission::Denied {
            self.channels[1] = Permission::Unknown;
        }
    }
    pub fn audio_received(&mut self, index: usize) {
        if index == 1
            || self.channels[index] != Permission::Denied
            || (self.query)()[index] == Permission::Unknown
        {
            self.channels[index] = Permission::Granted;
            self.failed[index] = false;
            if index == 0 {
                self.repeat_after_microphone_prompt = false;
                self.check_requested = false;
            }
        }
    }
    pub fn blocked(&self) -> Option<&'static str> {
        (self.channels[0] == Permission::Denied).then_some(MICROPHONE_DENIED)
    }
    pub fn system_warning(&self) -> Option<&'static str> {
        if self.channels[1] == Permission::Denied {
            Some("Нет доступа к системному звуку")
        } else if self.failed[1] {
            Some("Системный звук недоступен. Проверьте доступ")
        } else {
            None
        }
    }
    pub fn recording_status(
        &self,
        recording: &crate::state::Recording,
    ) -> Option<crate::state::RecordingStatus> {
        let mut status = recording.status()?;
        if recording.phase == crate::state::Phase::Recording && !recording.importing {
            if let Some(reason) = self.blocked().or_else(|| self.system_warning()) {
                status.text = reason.into();
                status.warning = true;
            } else if cfg!(target_os = "macos")
                && self.checked
                && self.channels[1] == Permission::Unknown
                && recording.silent_channels()[1]
            {
                status.text =
                    "Нет звука собеседников: тишина или нет доступа к системному звуку".into();
                status.warning = true;
            }
        }
        Some(status)
    }
    pub fn recording_help(&self, recording: &crate::state::Recording) -> bool {
        recording.phase == crate::state::Phase::Recording
            && !recording.importing
            && (self.needs_help()
                || cfg!(target_os = "macos")
                    && self.checked
                    && self.channels[1] == Permission::Unknown
                    && recording.silent_channels()[1])
    }
    pub fn needs_help(&self) -> bool {
        self.blocked().is_some() || self.system_warning().is_some()
    }
}
/// Open the privacy pane for the microphone, or on macOS for system audio
/// (Screen & System Audio Recording).
pub fn open_settings(ui: &mut egui::Ui, system: bool) {
    if crate::theme::button(ui, "Открыть настройки", crate::theme::ButtonStyle::Ghost).clicked()
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(if cfg!(windows) {
            "ms-settings:privacy-microphone"
        } else if system {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        } else {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
        }));
    }
}
pub fn blocked_action(ui: &mut egui::Ui, reason: Option<&str>) {
    if reason == Some(MICROPHONE_DENIED) {
        open_settings(ui, false);
    }
}
fn label(permission: Permission) -> &'static str {
    match permission {
        Permission::Granted => "доступ подтверждён",
        Permission::Denied => "доступ запрещён",
        Permission::NotAsked => "разрешение ещё не запрошено",
        Permission::Unknown => "доступ не определён; проверьте звук",
    }
}
pub fn panel(ui: &mut egui::Ui, access: &Access, all: bool, busy: bool) -> bool {
    if !all && !access.needs_help() {
        return false;
    }
    let mut check = false;
    crate::theme::callout(ui, crate::theme::icons::INFO, |ui| {
        if all {
            ui.label(if cfg!(windows) {
                "Для записи нужен доступ к микрофону."
            } else {
                "macOS: системный звук целиком, кроме notetaker."
            });
            ui.weak(format!("Микрофон: {}", label(access.channels[0])));
            ui.weak(format!("Системный звук: {}", label(access.channels[1])));
        }
        if let Some(reason) = access.blocked() {
            ui.label(reason);
            ui.weak(if cfg!(windows) {
                "Параметры → Конфиденциальность и защита → Микрофон."
            } else {
                "Системные настройки → Конфиденциальность и безопасность → Микрофон."
            });
            if !cfg!(windows) {
                ui.weak("После разрешения перезапустите notetaker.");
            }
        }
        if let Some(reason) = access.system_warning() {
            ui.label(if access.channels[1] == Permission::Denied {
                "Нет доступа к системному звуку: собеседники не запишутся"
            } else {
                reason
            });
            ui.weak("Системные настройки → Конфиденциальность и безопасность → Запись экрана и системного звука.");
            ui.weak("Если macOS попросит, перезапустите notetaker после разрешения.");
        }
        ui.horizontal_wrapped(|ui| {
            open_settings(
                ui,
                access.blocked().is_none() && access.system_warning().is_some(),
            );
            check = ui
                .add_enabled_ui(!busy, |ui| {
                    crate::theme::button(ui, "Проверить снова", crate::theme::ButtonStyle::Ghost)
                })
                .inner
                .clicked();
        });
    });
    check
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn denied_blocks_mic_but_not_system_and_unknown_is_not_a_denial() {
        let mut access = Access::default();
        for state in [
            Permission::Granted,
            Permission::Unknown,
            Permission::NotAsked,
        ] {
            access.channels = [state, Permission::Unknown];
            assert!(access.blocked().is_none());
            assert!(!access.needs_help());
        }
        access.channels = [Permission::Denied, Permission::Unknown];
        assert_eq!(access.blocked(), Some("Нет доступа к микрофону"));
        access.channels = [Permission::Granted, Permission::Denied];
        assert!(access.blocked().is_none());
        assert!(access.needs_help());
        let mut recording = crate::state::Recording::default();
        recording.phase = crate::state::Phase::Recording;
        let status = access.recording_status(&recording).unwrap();
        assert_eq!(status.text, "Нет доступа к системному звуку");
        assert!(status.warning);
    }
    #[test]
    fn focus_requeries_only_on_return_and_granted_clears_the_callout() {
        let mut access = Access::default();
        let calls = std::cell::Cell::new(0);
        for focused in [true, true, false, true] {
            access.on_focus(focused, || {
                calls.set(calls.get() + 1);
                [
                    if calls.get() == 1 {
                        Permission::Denied
                    } else {
                        Permission::Granted
                    },
                    Permission::Unknown,
                ]
            });
        }
        assert_eq!(calls.get(), 2);
        assert!(!access.needs_help());
        access.capture_failed(1);
        assert_eq!(access.channels[1], Permission::Unknown);
        assert!(access.needs_help());
        access.audio_received(1);
        assert_eq!(access.channels[1], Permission::Granted);
        assert!(!access.needs_help());
    }
    #[test]
    fn panel_is_absent_before_a_prompt_and_visible_for_a_denial() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let mut access = Access::default();
        access.channels[0] = Permission::NotAsked;
        let mut height = 0.0;
        crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            let y = ui.cursor().top();
            panel(ui, &access, false, false);
            height = ui.cursor().top() - y;
        })
        .drop_without_applying_deltas();
        assert_eq!(height, 0.0);
        access.channels[0] = Permission::Denied;
        let output = crate::test_ui::frame(&ctx, vec![], &mut |ui| {
            panel(ui, &access, false, false);
        });
        crate::test_ui::text_rect(&output.shapes, "Нет доступа к микрофону");
        crate::test_ui::text_rect(&output.shapes, "Открыть настройки");
        output.drop_without_applying_deltas();
    }
    #[test]
    fn permission_answer_requests_one_repeat_only() {
        let mut access = Access::default();
        access.channels[0] = Permission::NotAsked;
        access.repeat_after_microphone_prompt = true;
        access.apply_query([Permission::Granted, Permission::Unknown]);
        assert!(access.check_requested);
        access.check_requested = false;
        access.apply_query([Permission::Granted, Permission::Unknown]);
        assert!(!access.check_requested);
    }
    #[test]
    fn all_recording_entry_points_refuse_a_denied_microphone() {
        let (mut state, commands, _events) = crate::test_ui::state();
        state.ready = true;
        state.permissions.channels[0] = Permission::Denied;
        state.permissions.query = || [Permission::Denied, Permission::Unknown];
        assert!(!state.can_prepare_recording());
        assert!(state.can_prepare_import());
        state.start_recording();
        state.toggle_recording();
        assert_eq!(state.permissions.blocked(), Some(MICROPHONE_DENIED));
        assert!(!state.recording.busy());
        assert!(state.permissions.show_requested);
        assert!(
            commands
                .try_iter()
                .all(|cmd| !matches!(cmd, crate::worker::Command::Record(_)))
        );
    }
}
