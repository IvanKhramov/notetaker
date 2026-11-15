//! Speaker controls and selection are kept separate from text editing.
use std::collections::BTreeSet;

use eframe::egui;
use notetaker_core::{MeetingMeta, SpeakerEdit, SpeakerUndo, Transcript};
use uuid::Uuid;

#[derive(Default)]
pub struct SpeakerUi {
    pub cache_revision: u64,
    timeline: Option<(
        (std::time::Instant, notetaker_core::Revision, usize),
        std::sync::Arc<CachedTimeline>,
    )>,
    pub selected: BTreeSet<Uuid>,
    pub anchor: Option<usize>,
    pub scroll_to: Option<Uuid>,
    pub undo: Option<SpeakerUndo>,
    pub busy: bool,
    pub voice_status: String,
    pub recalculating: bool,
    pub pending_count: Option<Option<u32>>,
}
pub struct CachedTimeline {
    pub timeline: notetaker_core::SpeakerTimeline,
    pub remote_count: usize,
}
pub fn timeline(document: &mut crate::state::Document) -> std::sync::Arc<CachedTimeline> {
    let key = (
        document.changed_at,
        document.data.revisions.transcript,
        document.data.transcript.segments.len(),
    );
    if let Some((old, cached)) = &document.speaker_ui.timeline
        && old == &key
    {
        return cached.clone();
    }
    let cached = std::sync::Arc::new(CachedTimeline {
        timeline: document.data.transcript.speaker_timeline(),
        remote_count: remote_count(&document.data.transcript),
    });
    document.speaker_ui.timeline = Some((key, cached.clone()));
    cached
}
impl SpeakerUi {
    pub fn invalidate_transcript(&mut self) {
        self.cache_revision = self.cache_revision.wrapping_add(1);
        self.timeline = None;
    }
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

pub fn rename_menu(
    ui: &mut egui::Ui,
    transcript: &Transcript,
    meta: &MeetingMeta,
    id: &str,
    voices: Option<&notetaker_core::VoiceLibrary>,
    global_allowed: bool,
    anchor: (&egui::Response, bool),
) -> Option<SpeakerEdit> {
    let (anchor, show_actions) = anchor;
    let mut edit = None;
    // Quiet, frameless: a framed 32px button outweighed the speaker name.
    let more = crate::theme::icon_button_visible(
        ui,
        crate::theme::icons::ELLIPSIS,
        "Действия собеседника",
        show_actions
            || ui.ctx().content_rect().width() < 900.0
            || egui::Popup::is_any_open(ui.ctx()),
    );
    if (anchor.clicked() || more.clicked())
        && !egui::Popup::is_id_open(ui.ctx(), egui::Popup::menu(anchor).get_id())
    {
        ui.data_mut(|data| {
            data.insert_temp(
                egui::Id::unique(("speaker-rename-form", meta.id, id)),
                false,
            )
        });
    }
    egui::Popup::menu(anchor)
        .open_memory((anchor.clicked() || more.clicked()).then_some(egui::SetOpenCommand::Toggle))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(ui.available_width().min(300.0));
            let name_key = ui.make_persistent_id(("speaker-name", meta.id, id));
            let all_key = name_key.with("everywhere");
            let form_key = egui::Id::unique(("speaker-rename-form", meta.id, id));
            let mut renaming = ui
                .data_mut(|data| data.get_temp::<bool>(form_key))
                .unwrap_or(false);
            let mut name = ui
                .data_mut(|data| data.get_temp::<String>(name_key))
                .unwrap_or_else(|| transcript.speaker_name(id).into());
            let mut everywhere = ui
                .data_mut(|data| data.get_temp::<bool>(all_key))
                .unwrap_or(false);
            let activate = !renaming
                && crate::theme::menu_item(ui, "", "Переименовать", false, false).clicked();
            if activate {
                ui.input_mut(|input| {
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Space);
                });
                renaming = true;
                name = transcript.speaker_name(id).into();
                ui.data_mut(|data| data.insert_temp(form_key.with("focus"), true));
            }
            let focus = ui
                .data_mut(|data| data.remove_temp::<bool>(form_key.with("focus")))
                .unwrap_or(false);
            if !renaming {
                if let Some(voice) = transcript.speaker_voice_ids.get(id)
                    && crate::theme::menu_item(ui, "", "Открыть человека", false, false).clicked()
                {
                    crate::people::request(
                        ui,
                        crate::people::Request {
                            name: transcript.speaker_name(id).into(),
                            email: String::new(),
                            voice: Some(voice.clone()),
                        },
                    );
                    ui.close();
                }
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
                        crate::theme::Checkbox::new(
                            &mut everywhere,
                            "Применить ко всем встречам с этим голосом",
                        ),
                    );
                    if !global_allowed {
                        everywhere = false;
                        ui.weak("Дождитесь сохранения правок");
                    }
                } else {
                    everywhere = false;
                }
            }
            if !global_allowed {
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
            if renaming {
                if focus {
                    let mut state =
                        egui::TextEdit::load_state(ui.ctx(), name_key).unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(name.chars().count()),
                        )));
                    state.store(ui.ctx(), name_key);
                }
                let enter = !focus
                    && ui.memory(|memory| memory.has_focus(name_key))
                    && ui.input_mut(|input| {
                        input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    });
                let escape = ui
                    .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
                let field = crate::theme::framed_text_field(ui, &mut name, name_key);
                if focus {
                    field.request_focus();
                }
                if voices.is_some_and(|voices| {
                    !voices.name_available(
                        &name,
                        transcript.speaker_voice_ids.get(id).map(String::as_str),
                        id == "me" || transcript.self_speaker.as_deref() == Some(id),
                    )
                }) {
                    ui.weak(notetaker_core::VOICE_NAME_TAKEN);
                }
                let mut apply = false;
                let mut cancel = escape;
                crate::theme::footer(ui, |ui| {
                    apply = ui
                        .add_enabled(
                            !name.trim().is_empty(),
                            crate::theme::styled_button(
                                crate::theme::Palette::of(ui),
                                "Переименовать",
                                crate::theme::ButtonStyle::Primary,
                            ),
                        )
                        .clicked();
                    cancel |= crate::theme::button(ui, "Отмена", crate::theme::ButtonStyle::Ghost)
                        .clicked();
                });
                if cancel {
                    renaming = false;
                    name = transcript.speaker_name(id).into();
                    ui.close();
                } else if (enter || apply) && !name.trim().is_empty() {
                    renaming = false;
                    choose(ui, name.clone(), false);
                }
            } else {
                if transcript.ktalk_sources.contains_key(id) {
                    crate::ktalk::merge_menu(ui, transcript.speaker_name(id));
                }
                if let Some(suggestion) = transcript.speaker_suggestions.get(id)
                    && crate::theme::button(
                        ui,
                        &format!("Подтвердить: {suggestion}"),
                        crate::theme::ButtonStyle::Ghost,
                    )
                    .clicked()
                {
                    choose(ui, suggestion.clone(), false);
                }
                if let Some(event) = &meta.calendar_event {
                    for attendee in &event.attendees {
                        if crate::theme::button(
                            ui,
                            attendee.label(),
                            crate::theme::ButtonStyle::Ghost,
                        )
                        .clicked()
                        {
                            choose(ui, attendee.label().into(), false);
                        }
                    }
                }
                if voices.is_some()
                    && crate::theme::button(ui, "Это я", crate::theme::ButtonStyle::Ghost).clicked()
                {
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
            }
            ui.data_mut(|data| data.insert_temp(form_key, renaming));
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
    editable: bool,
) -> (Option<Option<u32>>, Option<SpeakerEdit>) {
    use crate::theme::{self, ButtonStyle, Palette};
    let mut apply = None;
    let mut edit = None;
    let cached = timeline(document);
    let timeline = &cached.timeline;
    if !document.speaker_ui.voice_status.is_empty() {
        ui.weak(&document.speaker_ui.voice_status);
    }
    ui.horizontal_wrapped(|ui| {
        let response = if unavailable.is_some() {
            ui.label(
                egui::RichText::new(crate::wording::count(
                    cached.remote_count as u64,
                    crate::wording::SPEAKERS,
                ))
                .size(12.0)
                .color(Palette::of(ui).tertiary),
            )
        } else {
            ui.add_enabled_ui(true, |ui| {
                let label =
                    crate::wording::count(cached.remote_count as u64, crate::wording::SPEAKERS);
                let button = theme::styled_button(Palette::of(ui), &label, ButtonStyle::Ghost)
                    .right_text(" ");
                let (response, _) = theme::menu_button(ui, button, |ui| {
                    ui.set_width(140.0);
                    for count in std::iter::once(None).chain((1..=8).map(Some)) {
                        let label = count.map_or_else(|| "Авто".into(), |n| n.to_string());
                        if theme::menu_item(
                            ui,
                            "",
                            &label,
                            false,
                            count == Some(remote_count(&document.data.transcript) as u32),
                        )
                        .clicked()
                        {
                            document.speaker_ui.pending_count = Some(count);
                            ui.close();
                        }
                    }
                });
                // Draw the chevron: the embedded text font has no ▾ glyph.
                let c = response.rect.right_center() - egui::vec2(11.0, 0.0);
                crate::theme::paint_icon(
                    ui,
                    c,
                    crate::theme::icons::CHEVRON_DOWN,
                    16.0,
                    Palette::of(ui).muted,
                );
                response
            })
            .inner
        };
        if let Some(reason) = unavailable {
            response.on_hover_text(reason);
        }
        if can_merge(timeline) {
            ui.add_enabled_ui(editable, |ui| merge_menu(ui, timeline, &mut edit));
        }
        if document.speaker_ui.recalculating {
            ui.weak("Пересчитываю собеседников…");
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
    (apply, edit)
}

fn can_merge(timeline: &notetaker_core::SpeakerTimeline) -> bool {
    timeline.speakers.len() >= 2
}

pub fn controls(
    ui: &mut egui::Ui,
    transcript: &Transcript,
    meta: &MeetingMeta,
    state: &mut SpeakerUi,
    timeline: &notetaker_core::SpeakerTimeline,
) -> Option<SpeakerEdit> {
    let edit = None;
    if state.undo.is_none() && state.selected.is_empty() {
        return edit;
    }
    selection_controls(ui, transcript, meta, state, timeline, edit)
}

fn merge_menu(
    ui: &mut egui::Ui,
    timeline: &notetaker_core::SpeakerTimeline,
    edit: &mut Option<SpeakerEdit>,
) {
    {
        crate::theme::menu_button(
            ui,
            crate::theme::styled_button(
                crate::theme::Palette::of(ui),
                "Объединить собеседников…",
                crate::theme::ButtonStyle::Ghost,
            ),
            |ui| {
                for speaker in &timeline.speakers {
                    ui.menu_button(&speaker.name, |ui| {
                        for other in &timeline.speakers {
                            if other.id != speaker.id
                                && crate::theme::button(
                                    ui,
                                    &other.name,
                                    crate::theme::ButtonStyle::Ghost,
                                )
                                .clicked()
                            {
                                *edit = Some(SpeakerEdit::Merge {
                                    from: speaker.id.clone(),
                                    into: other.id.clone(),
                                });
                                ui.close();
                            }
                        }
                    });
                }
            },
        );
    }
}

fn selection_controls(
    ui: &mut egui::Ui,
    transcript: &Transcript,
    meta: &MeetingMeta,
    state: &mut SpeakerUi,
    timeline: &notetaker_core::SpeakerTimeline,
    mut edit: Option<SpeakerEdit>,
) -> Option<SpeakerEdit> {
    ui.horizontal_wrapped(|ui| {
        if let Some(undo) = &state.undo
            && crate::theme::button(ui, "Отменить объединение", crate::theme::ButtonStyle::Ghost)
                .clicked()
        {
            edit = Some(SpeakerEdit::Undo(Box::new(undo.clone())));
        }
        if state.selected.is_empty() {
            return;
        }
        ui.weak(format!(
            "Выбрано: {}",
            crate::wording::count(state.selected.len() as u64, crate::wording::UTTERANCES)
        ));
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
                    if crate::theme::button(ui, &speaker.name, crate::theme::ButtonStyle::Ghost)
                        .clicked()
                    {
                        assign(ui, speaker.id.clone(), None);
                    }
                }
                if crate::theme::button(ui, "Новый собеседник", crate::theme::ButtonStyle::Ghost)
                    .clicked()
                {
                    assign(ui, transcript.new_speaker_id(), None);
                }
                if let Some(event) = &meta.calendar_event {
                    ui.separator();
                    ui.weak("Участники календаря");
                    for attendee in &event.attendees {
                        if crate::theme::button(
                            ui,
                            attendee.label(),
                            crate::theme::ButtonStyle::Ghost,
                        )
                        .clicked()
                        {
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
            if crate::theme::button(ui, "Снять выбор", crate::theme::ButtonStyle::Ghost).clicked()
            {
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
    fn microphone_and_one_system_speaker_can_be_merged() {
        let mut transcript = Transcript::default();
        for (speaker, channel) in [("me", Channel::Mic), ("s1", Channel::System)] {
            transcript.segments.push(Segment {
                id: Uuid::now_v7(),
                speaker: SpeakerId(speaker.into()),
                channel,
                start_ms: 0,
                end_ms: 100,
                text: String::new(),
                source: Source::Final,
                edited: false,
                lang: None,
            });
        }
        assert_eq!(remote_count(&transcript), 1);
        assert!(can_merge(&transcript.speaker_timeline()));
        transcript.segments.pop();
        assert!(!can_merge(&transcript.speaker_timeline()));
    }
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

#[cfg(test)]
mod direct_rename_tests {
    use super::*;
    #[test]
    fn name_menu_renames_with_focus_and_only_links_known_voices() {
        for linked in [false, true] {
            let ctx = egui::Context::default();
            crate::theme::install(&ctx);
            let mut transcript = Transcript::default();
            transcript
                .speaker_names
                .insert("s1".into(), "Алексей".into());
            if linked {
                transcript
                    .speaker_voice_ids
                    .insert("s1".into(), "voice-1".into());
            }
            let meta = MeetingMeta::new("Встреча", chrono::Utc::now());
            let mut edit = None;
            let mut draw = |ui: &mut egui::Ui| {
                let anchor = ui.add(egui::Label::new("Алексей").sense(egui::Sense::click()));
                if let Some(next) =
                    rename_menu(ui, &transcript, &meta, "s1", None, true, (&anchor, false))
                {
                    edit = Some(next);
                }
            };
            crate::test_ui::click(&ctx, &mut draw, "Алексей");
            let output = crate::test_ui::frame(&ctx, vec![], &mut draw);
            crate::test_ui::text_rect(&output.shapes, "Переименовать");
            let person=output.shapes.iter().any(|s|matches!(&s.shape,egui::epaint::Shape::Text(text) if text.galley.text()=="Открыть человека"));
            assert_eq!(person, linked);
            output.drop_without_applying_deltas();
            crate::test_ui::click(&ctx, &mut draw, "Переименовать");
            let focus = ctx.memory(|m| m.focused()).expect("rename field focused");
            assert!(egui::TextEdit::load_state(&ctx, focus).is_some());
            crate::test_ui::frame(
                &ctx,
                vec![
                    egui::Event::Key {
                        key: egui::Key::A,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::COMMAND,
                    },
                    egui::Event::Text("Новое имя".into()),
                ],
                &mut draw,
            )
            .drop_without_applying_deltas();
            crate::test_ui::click(&ctx, &mut draw, "Переименовать");
            assert!(matches!(edit,Some(SpeakerEdit::Rename {name,..}) if name=="Новое имя"));
        }
    }
}
