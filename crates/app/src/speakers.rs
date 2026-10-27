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

pub fn color(id: &str) -> Color32 {
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(91, 161, 224),
        Color32::from_rgb(211, 144, 76),
        Color32::from_rgb(106, 184, 135),
        Color32::from_rgb(185, 129, 210),
        Color32::from_rgb(215, 113, 136),
        Color32::from_rgb(90, 184, 185),
        Color32::from_rgb(184, 170, 86),
        Color32::from_rgb(151, 155, 218),
    ];
    let hash = id.bytes().fold(0usize, |hash, b| {
        hash.wrapping_mul(31).wrapping_add(b as usize)
    });
    COLORS[hash % COLORS.len()]
}
fn duration(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, ms / 1000 % 60)
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
    ui.menu_button(RichText::new(title).color(color(id)), |ui| {
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

pub fn controls(
    ui: &mut egui::Ui,
    transcript: &Transcript,
    meta: &MeetingMeta,
    state: &mut SpeakerUi,
    voices: Option<&notetaker_core::VoiceLibrary>,
    global_allowed: bool,
) -> Option<SpeakerEdit> {
    let mut edit = None;
    let timeline = transcript.speaker_timeline();
    egui::CollapsingHeader::new("Спикеры - кто сколько говорил").id_salt(meta.id).default_open(true).show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt((meta.id,"speaker-stats")).max_height(160.0).show(ui, |ui| {
            egui::Grid::new((meta.id,"speaker-table")).striped(true).show(ui, |ui| {
                for heading in ["Спикер", "Время", "Доля", "Реплики", "Ходы", "Длинный монолог", ""] { ui.weak(heading); }
                ui.end_row();
                for speaker in &timeline.speakers {
                    ui.push_id(&speaker.id, |ui| {
                        if let Some(rename) = rename_menu(ui,transcript,meta,&speaker.id,voices,global_allowed) { edit = Some(rename); }
                    });
                    ui.label(duration(speaker.total_ms));
                    ui.label(format!("{:.1}%",speaker.share_percent));
                    ui.label(speaker.segment_count.to_string());
                    ui.label(speaker.turns.to_string());
                    ui.label(duration(speaker.longest_monologue_ms));
                    ui.menu_button("Объединить с...", |ui| {
                        for other in &timeline.speakers {
                            if other.id != speaker.id && ui.button(RichText::new(&other.name).color(color(&other.id))).clicked() {
                                edit = Some(SpeakerEdit::Merge { from:speaker.id.clone(), into:other.id.clone() });
                                ui.close();
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        });
        ui.small("Доля от суммы времени спикеров. Пересечения засчитываются каждому. Ход: соседние реплики одного спикера с паузой до 1 с.");
    });
    ui.horizontal_wrapped(|ui| {
        if let Some(undo) = &state.undo
            && ui.button("Отменить объединение").clicked()
        {
            edit = Some(SpeakerEdit::Undo(Box::new(undo.clone())));
        }
        ui.label(format!("Выбрано реплик: {}", state.selected.len()));
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
    ui.small("Нажмите время реплики для выбора; Shift - диапазон, Ctrl - добавить или убрать.");
    egui::CollapsingHeader::new("Таймлайн")
        .id_salt((meta.id, "timeline"))
        .default_open(true)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt((meta.id, "timeline-lanes"))
                .max_height(120.0)
                .show(ui, |ui| {
                    let width = ui.available_width().max(1.0);
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(width, timeline.speakers.len() as f32 * 24.0),
                        egui::Sense::hover(),
                    );
                    let label_width = (width * 0.25).min(130.0);
                    let bar_width = (width - label_width).max(1.0);
                    for (lane, speaker) in timeline.speakers.iter().enumerate() {
                        let top = rect.top() + lane as f32 * 24.0;
                        let label_rect = egui::Rect::from_min_size(
                            egui::pos2(rect.left(), top),
                            egui::vec2(label_width - 4.0, 22.0),
                        );
                        ui.painter().with_clip_rect(label_rect).text(
                            label_rect.left_center(),
                            egui::Align2::LEFT_CENTER,
                            &speaker.name,
                            egui::FontId::proportional(12.0),
                            color(&speaker.id),
                        );
                        let left = rect.left() + label_width;
                        ui.painter().line_segment(
                            [
                                egui::pos2(left, top + 20.0),
                                egui::pos2(rect.right(), top + 20.0),
                            ],
                            egui::Stroke::new(1.0, ui.visuals().faint_bg_color),
                        );
                        for segment in timeline.segments.iter().filter(|s| s.speaker == speaker.id)
                        {
                            let x = |ms| {
                                left + bar_width
                                    * (ms as f64 / timeline.duration_ms.max(1) as f64) as f32
                            };
                            let bar = egui::Rect::from_min_max(
                                egui::pos2(x(segment.start_ms), top + 3.0),
                                egui::pos2(
                                    x(segment.end_ms)
                                        .max(x(segment.start_ms) + 2.0)
                                        .min(rect.right()),
                                    top + 19.0,
                                ),
                            );
                            ui.painter().rect_filled(bar, 2, color(&speaker.id));
                            if ui
                                .interact(
                                    bar,
                                    ui.scope_id().with(("timeline-bar", segment.id)),
                                    egui::Sense::click(),
                                )
                                .on_hover_text(format!(
                                    "{} {}-{}\n{}",
                                    segment.name,
                                    duration(segment.start_ms),
                                    duration(segment.end_ms),
                                    segment.text
                                ))
                                .clicked()
                            {
                                state.scroll_to = Some(segment.id);
                            }
                        }
                    }
                });
            ui.small(format!("0:00 - {}", duration(timeline.duration_ms)));
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
