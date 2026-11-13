//! Speaker controls and selection are kept separate from text editing.
use std::collections::BTreeSet;

use eframe::egui::{self, Color32, RichText};
use notetaker_core::{MeetingMeta, SpeakerEdit, SpeakerUndo, Transcript};
use uuid::Uuid;

#[derive(Default)]
pub struct SpeakerUi {
    pub selected: BTreeSet<Uuid>,
    pub anchor: Option<usize>,
    pub scroll_to: Option<Uuid>,
    pub undo: Option<SpeakerUndo>,
    pub busy: bool,
    pub recalculating: bool,
    pub pending_count: Option<Option<u32>>,
}
impl SpeakerUi {
    pub fn select(&mut self, transcript: &Transcript, index: usize, shift: bool, ctrl: bool) {
        let Some(segment) = transcript.segments.get(index) else {
            return;
        };
        if shift {
            let anchor = self
                .anchor
                .unwrap_or(index)
                .min(transcript.segments.len() - 1);
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(
                transcript.segments[anchor.min(index)..=anchor.max(index)]
                    .iter()
                    .map(|s| s.id),
            );
            self.anchor = Some(anchor);
        } else if ctrl {
            if !self.selected.remove(&segment.id) {
                self.selected.insert(segment.id);
            }
            self.anchor = Some(index);
        } else {
            self.selected.clear();
            self.selected.insert(segment.id);
            self.anchor = Some(index);
        }
    }
}

pub fn color(_id: &str) -> Color32 {
    Color32::from_rgb(138, 135, 127)
}

fn label<'a>(text: &'a Transcript, id: &'a str) -> &'a str {
    match text.speaker_name(id) {
        "me" => "Я",
        "them" => "Собеседники",
        name => name,
    }
}

pub fn rename_menu(
    ui: &mut egui::Ui,
    transcript: &Transcript,
    meta: &MeetingMeta,
    id: &str,
    voices: Option<&notetaker_core::VoiceLibrary>,
    global_allowed: bool,
) -> Option<SpeakerEdit> {
    let mut edit = None;
    let title = transcript.speaker_suggestions.get(id).map_or_else(
        || label(transcript, id).to_owned(),
        |name| format!("{name}?"),
    );
    if ui.link(RichText::new(title).color(color(id))).clicked() {
        crate::people::request(
            ui,
            crate::people::Request {
                name: transcript.speaker_name(id).into(),
                email: String::new(),
                voice: transcript.speaker_voice_ids.get(id).cloned(),
            },
        );
    }
    // Quiet, frameless: a framed 32px button outweighed the speaker name.
    let more = crate::theme::styled_button(
        crate::theme::Palette::of(ui),
        "…",
        crate::theme::ButtonStyle::Ghost,
    )
    .small();
    egui::containers::menu::MenuButton::from_button(more).ui(ui, |ui| {
        let name_key = ui.make_persistent_id(("speaker-name", meta.id, id));
        let all_key = name_key.with("everywhere");
        let mut name = ui
            .data_mut(|data| data.get_temp::<String>(name_key))
            .unwrap_or_else(|| transcript.speaker_name(id).into());
        let mut everywhere = ui
            .data_mut(|data| data.get_temp::<bool>(all_key))
            .unwrap_or(false);
        if voices.is_some_and(|voices| {
            transcript.speaker_voice_ids.get(id).is_some_and(|linked| {
                voices
                    .voices
                    .iter()
                    .any(|voice| voice.id.as_ref() == Some(linked))
            })
        }) {
            ui.add_enabled(
                global_allowed,
                egui::Checkbox::new(&mut everywhere, "Применить ко всем встречам с этим голосом"),
            );
            if !global_allowed {
                everywhere = false;
                ui.weak("Дождитесь сохранения правок");
            }
        } else {
            everywhere = false;
        }
        let mut choose = |ui: &mut egui::Ui, name: String, is_me: bool| {
            edit = Some(SpeakerEdit::Rename {
                speaker: id.into(),
                name,
                is_me,
                everywhere,
            });
            ui.close();
        };
        if let Some(suggestion) = transcript.speaker_suggestions.get(id)
            && ui.button(format!("Подтвердить: {suggestion}")).clicked()
        {
            choose(ui, suggestion.clone(), false);
        }
        if let Some(event) = &meta.calendar_event {
            for attendee in &event.attendees {
                if ui.button(attendee.label()).clicked() {
                    choose(ui, attendee.label().into(), false);
                }
            }
        }
        ui.text_edit_singleline(&mut name);
        if ui
            .add_enabled(!name.trim().is_empty(), egui::Button::new("Назвать"))
            .clicked()
        {
            choose(ui, name.clone(), false);
        }
        if voices.is_some() && ui.button("Это я").clicked() {
            choose(
                ui,
                transcript
                    .speaker_names
                    .get(id)
                    .or_else(|| transcript.speaker_suggestions.get(id))
                    .cloned()
                    .unwrap_or_else(|| "Я".into()),
                true,
            );
        }
        if let Some(SpeakerEdit::Rename { name: next, .. }) = &edit {
            name.clone_from(next);
            everywhere = false;
        }
        ui.data_mut(|data| {
            data.insert_temp(name_key, name);
            data.insert_temp(all_key, everywhere);
        });
    });
    edit
}

/// The count describes remote transcript IDs, independent of the microphone.
fn remote_count(transcript: &Transcript) -> usize {
    transcript
        .segments
        .iter()
        .filter(|s| s.channel == notetaker_core::Channel::System)
        .map(|s| &s.speaker.0)
        .collect::<BTreeSet<_>>()
        .len()
}

pub fn count_control(
    ui: &mut egui::Ui,
    document: &mut crate::state::Document,
    unavailable: Option<&str>,
) -> Option<Option<u32>> {
    use crate::theme::{self, ButtonStyle, Palette};
    let mut apply = None;
    ui.horizontal_wrapped(|ui| {
        let response = ui
            .add_enabled_ui(unavailable.is_none(), |ui| {
                let label = format!("Собеседников: {}", remote_count(&document.data.transcript));
                let button = theme::styled_button(Palette::of(ui), &label, ButtonStyle::Ghost)
                    .right_text(" ");
                let (response, _) =
                    egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                        ui.set_width(140.0);
                        for count in std::iter::once(None).chain((1..=8).map(Some)) {
                            let label = count.map_or_else(|| "Авто".into(), |n| n.to_string());
                            if theme::nav_row(ui, &label, false).clicked() {
                                document.speaker_ui.pending_count = Some(count);
                                ui.close();
                            }
                        }
                    });
                // Draw the chevron: the embedded text font has no ▾ glyph.
                let c = response.rect.right_center() - egui::vec2(11.0, 0.0);
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        c + egui::vec2(-3.0, -1.5),
                        c + egui::vec2(3.0, -1.5),
                        c + egui::vec2(0.0, 2.0),
                    ],
                    ui.visuals().weak_text_color(),
                    egui::Stroke::NONE,
                ));
                response
            })
            .inner;
        if let Some(reason) = unavailable {
            response.on_disabled_hover_text(reason);
        }
        if document.speaker_ui.recalculating {
            ui.weak("Пересчитываю спикеров…");
        }
    });
    if let Some(count) = document.speaker_ui.pending_count {
        ui.weak("Имена собеседников и связи с голосами будут сброшены.");
        ui.horizontal(|ui| {
            let label =
                count.map_or_else(|| "Применить: авто".into(), |n| format!("Применить: {n}"));
            if ui
                .add_enabled(
                    unavailable.is_none(),
                    theme::styled_button(Palette::of(ui), &label, ButtonStyle::Ghost),
                )
                .clicked()
            {
                apply = Some(count);
                document.speaker_ui.pending_count = None;
            }
            if theme::button(ui, "Отмена", ButtonStyle::Ghost).clicked() {
                document.speaker_ui.pending_count = None;
            }
        });
    }
    ui.add_space(8.0);
    apply
}

pub fn controls(
    ui: &mut egui::Ui,
    transcript: &Transcript,
    meta: &MeetingMeta,
    state: &mut SpeakerUi,
) -> Option<SpeakerEdit> {
    let mut edit = None;
    let timeline = transcript.speaker_timeline();
    ui.horizontal_wrapped(|ui| {
        egui::containers::menu::MenuButton::from_button(crate::theme::styled_button(
            crate::theme::Palette::of(ui),
            "Объединить собеседников…",
            crate::theme::ButtonStyle::Ghost,
        ))
        .ui(ui, |ui| {
            for speaker in &timeline.speakers {
                ui.menu_button(&speaker.name, |ui| {
                    for other in &timeline.speakers {
                        if other.id != speaker.id && ui.button(&other.name).clicked() {
                            edit = Some(SpeakerEdit::Merge {
                                from: speaker.id.clone(),
                                into: other.id.clone(),
                            });
                            ui.close();
                        }
                    }
                });
            }
        });
    });
    if state.undo.is_none() && state.selected.is_empty() {
        return edit;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(undo) = &state.undo
            && ui.button("Отменить объединение").clicked()
        {
            edit = Some(SpeakerEdit::Undo(Box::new(undo.clone())));
        }
        if state.selected.is_empty() {
            return;
        }
        ui.weak(format!("Выбрано реплик: {}", state.selected.len()));
        ui.add_enabled_ui(!state.selected.is_empty(), |ui| {
            ui.menu_button("Это говорил...", |ui| {
                let mut assign = |ui: &mut egui::Ui, speaker: String, name: Option<String>| {
                    edit = Some(SpeakerEdit::Reassign {
                        segments: state.selected.iter().copied().collect(),
                        speaker,
                        name,
                    });
                    ui.close();
                };
                for speaker in &timeline.speakers {
                    if ui
                        .button(RichText::new(&speaker.name).color(color(&speaker.id)))
                        .clicked()
                    {
                        assign(ui, speaker.id.clone(), None);
                    }
                }
                if ui.button("Новый спикер").clicked() {
                    assign(ui, transcript.new_speaker_id(), None);
                }
                if let Some(event) = &meta.calendar_event {
                    ui.separator();
                    ui.weak("Участники календаря");
                    for attendee in &event.attendees {
                        if ui.button(attendee.label()).clicked() {
                            let id = transcript
                                .speaker_names
                                .iter()
                                .find(|(_, name)| name.as_str() == attendee.label())
                                .map(|(id, _)| id.clone())
                                .unwrap_or_else(|| transcript.new_speaker_id());
                            assign(ui, id, Some(attendee.label().into()));
                        }
                    }
                }
            });
            if ui.button("Снять выбор").clicked() {
                state.selected.clear();
                state.anchor = None;
            }
        });
    });
    edit
}

#[cfg(test)]
mod tests {
    use super::*;
    use notetaker_core::{Channel, Segment, Source, SpeakerId};
    #[test]
    fn selection_supports_click_shift_and_ctrl_ranges() {
        let mut transcript = Transcript::default();
        for _ in 0..5 {
            transcript.segments.push(Segment {
                id: Uuid::now_v7(),
                speaker: SpeakerId("s1".into()),
                channel: Channel::System,
                start_ms: 0,
                end_ms: 100,
                text: String::new(),
                source: Source::Final,
                edited: false,
                lang: None,
            });
        }
        let ids: Vec<_> = transcript.segments.iter().map(|s| s.id).collect();
        let mut state = SpeakerUi::default();
        state.select(&transcript, 1, false, false);
        state.select(&transcript, 3, true, false);
        assert_eq!(state.selected, ids[1..=3].iter().copied().collect());
        state.select(&transcript, 2, false, true);
        assert!(!state.selected.contains(&ids[2]));
        state.select(&transcript, 4, true, true);
        assert_eq!(state.selected, ids[1..=4].iter().copied().collect());
        state.select(&transcript, 0, false, false);
        assert_eq!(state.selected, BTreeSet::from([ids[0]]));
    }
}
