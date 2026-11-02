use eframe::egui::{self, Color32, FontData, FontFamily, FontId, Stroke, TextStyle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    #[default]
    Light,
    Dark,
}
impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Системная",
            Self::Light => "Светлая",
            Self::Dark => "Тёмная",
        }
    }
    pub fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        });
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub card: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub border: Color32,
    pub soft: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub red: Color32,
    pub me: Color32,
}
impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        let rgb = Color32::from_rgb;
        if dark {
            Self {
                bg: rgb(30, 30, 28),
                sidebar: rgb(25, 25, 23),
                card: rgb(30, 30, 28),
                text: rgb(235, 234, 230),
                muted: rgb(160, 157, 148),
                border: rgb(57, 56, 51),
                soft: rgb(38, 38, 35),
                hover: rgb(46, 46, 41),
                selected: rgb(52, 52, 46),
                accent: rgb(105, 184, 116),
                on_accent: rgb(18, 36, 21),
                red: rgb(189, 105, 94),
                me: rgb(38, 38, 35),
            }
        } else {
            Self {
                bg: Color32::WHITE,
                sidebar: rgb(247, 246, 243),
                card: Color32::WHITE,
                text: rgb(31, 31, 28),
                muted: rgb(138, 135, 127),
                border: rgb(230, 228, 223),
                soft: rgb(247, 246, 243),
                hover: rgb(240, 239, 234),
                selected: rgb(235, 234, 228),
                accent: rgb(47, 122, 61),
                on_accent: Color32::WHITE,
                red: rgb(163, 76, 64),
                me: rgb(247, 246, 243),
            }
        }
    }
    pub fn of(ui: &egui::Ui) -> Self {
        Self::for_dark(ui.visuals().dark_mode)
    }
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("heading".into()))
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../assets/Inter-Regular.ttf"
        ))),
    );
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, "Inter".into());
    fonts.font_data.insert(
        "Inter Semibold".into(),
        std::sync::Arc::new(FontData::from_static(include_bytes!(
            "../assets/Inter-SemiBold.ttf"
        ))),
    );
    fonts.families.insert(
        FontFamily::Name("heading".into()),
        vec!["Inter Semibold".into()],
    );
    // Retain the monospace face and provide Cyrillic/symbol fallback.
    fonts
        .families
        .get_mut(&FontFamily::Monospace)
        .unwrap()
        .push("Inter".into());
    ctx.set_fonts(fonts);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let p = Palette::for_dark(theme == egui::Theme::Dark);
        let mut style = egui::Style::default();
        style.spacing.item_spacing = egui::vec2(8.0, 9.0);
        style.spacing.button_padding = egui::vec2(12.0, 8.0);
        style.spacing.interact_size.y = 32.0;
        style.spacing.window_margin = egui::Margin::same(20);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.extra_text_line_spacing = 3.0;
        style.spacing.scroll.bar_width = 5.0;
        style.spacing.scroll.floating_width = 3.0;
        style.spacing.scroll.bar_inner_margin = 3.0;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(11.5));
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(24.0, FontFamily::Name("heading".into())),
        );
        let v = &mut style.visuals;
        *v = if theme == egui::Theme::Dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.override_text_color = Some(p.text);
        v.weak_text_color = Some(p.muted);
        v.panel_fill = p.bg;
        v.window_fill = p.card;
        v.window_stroke = Stroke::new(1.0, p.border);
        v.window_corner_radius = 12.into();
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 2,
            color: Color32::from_black_alpha(if v.dark_mode { 72 } else { 24 }),
        };
        v.hyperlink_color = p.text;
        v.text_cursor.stroke = Stroke::new(1.5, p.text);
        v.menu_corner_radius = 8.into();
        v.extreme_bg_color = p.card;
        v.faint_bg_color = p.sidebar;
        v.code_bg_color = p.soft;
        v.warn_fg_color = p.red;
        v.error_fg_color = p.red;
        v.selection.bg_fill = p.selected;
        v.selection.stroke = Stroke::new(1.0, p.text);
        for widget in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            widget.corner_radius = 8.into();
            widget.bg_fill = p.soft;
            widget.weak_bg_fill = p.soft;
            widget.bg_stroke = Stroke::new(1.0, p.border);
            widget.fg_stroke = Stroke::new(1.0, p.text);
        }
        v.widgets.noninteractive.bg_fill = p.card;
        v.widgets.noninteractive.weak_bg_fill = p.card;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.hovered.bg_fill = p.hover;
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.active.bg_fill = p.selected;
        v.widgets.active.weak_bg_fill = p.selected;
        v.widgets.open.bg_fill = p.selected;
        ctx.set_style_of(theme, style);
    }
}

/// Keep dialogs inside the app viewport, including their frame and title bar.
pub fn window<'a>(ctx: &egui::Context, title: impl egui::IntoAtoms<'a>) -> egui::Window<'a> {
    let bounds = ctx.content_rect().shrink(16.0);
    egui::Window::new(title)
        .default_pos(bounds.center() - egui::vec2(280.0, 260.0))
        .max_width(bounds.width())
        .max_height(bounds.height())
        .constrain_to(bounds)
}

pub enum ButtonStyle {
    Primary,
    Secondary,
    Ghost,
    Stop,
}
pub fn button(ui: &mut egui::Ui, text: &str, style: ButtonStyle) -> egui::Response {
    ui.add(styled_button(Palette::of(ui), text, style))
}
pub fn styled_button(p: Palette, text: &str, style: ButtonStyle) -> egui::Button<'_> {
    let (fill, ink, border) = match style {
        ButtonStyle::Stop => (
            Color32::from_rgb(22, 22, 20),
            Color32::WHITE,
            Stroke::new(1.0, p.muted.gamma_multiply(0.55)),
        ),
        ButtonStyle::Primary => (p.text, p.card, Stroke::NONE),
        ButtonStyle::Secondary => (p.soft, p.text, Stroke::new(1.0, p.border)),
        ButtonStyle::Ghost => (Color32::TRANSPARENT, p.muted, Stroke::NONE),
    };
    egui::Button::new(egui::RichText::new(text).color(ink))
        .fill(fill)
        .stroke(border)
        .corner_radius(8)
}

pub fn section_caption(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(12.0)
            .color(Palette::of(ui).muted),
    );
}
pub fn page_heading(ui: &mut egui::Ui, text: &str) {
    ui.heading(text);
    ui.add_space(20.0);
}
/// Quiet list group, shared by secondary screens and dialogs.
pub fn card(p: Palette, selected: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(if selected { p.selected } else { p.card })
        .inner_margin(egui::Margin::symmetric(0, 10))
}

pub fn chip_frame(p: Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.soft)
        .stroke(Stroke::NONE)
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(9, 4))
}

pub fn chip(ui: &mut egui::Ui, text: &str) {
    let p = Palette::of(ui);
    chip_frame(p).show(ui, |ui| {
        ui.add_sized(
            [0.0, 18.0],
            egui::Label::new(egui::RichText::new(text).size(11.5).color(p.muted)),
        );
    });
}
pub fn filter_chip(
    ui: &mut egui::Ui,
    text: &str,
    selected: bool,
    dot: Option<Color32>,
) -> egui::Response {
    let p = Palette::of(ui);
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().button_padding =
                egui::vec2(if dot.is_some() { 18.0 } else { 9.0 }, 5.0);
            ui.spacing_mut().interact_size.y = 25.0;
            ui.add(
                egui::Button::new(egui::RichText::new(text).size(11.5).color(if selected {
                    p.text
                } else {
                    p.muted
                }))
                .fill(if selected { p.selected } else { p.soft })
                .stroke(Stroke::NONE)
                .corner_radius(20),
            )
        })
        .inner;
    if let Some(color) = dot {
        ui.painter().circle_filled(
            egui::pos2(response.rect.left() + 10.0, response.rect.center().y),
            3.0,
            color,
        );
    }
    response
}
pub fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, selected: &mut T, values: &[(T, &str)]) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        for &(value, text) in values {
            if filter_chip(ui, text, *selected == value, None).clicked() {
                *selected = value;
            }
        }
    });
}
/// Animated recording state. It is the only colored ornament in navigation.
pub fn waveform(ui: &mut egui::Ui) {
    let p = Palette::of(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
    let time = ui.input(|i| i.time) as f32;
    for n in 0..5 {
        let height = 4.0 + 12.0 * (time * 3.0 + n as f32 * 1.2).sin().abs();
        let x = rect.left() + 3.0 + n as f32 * 4.0;
        ui.painter().line_segment(
            [
                egui::pos2(x, rect.center().y - height / 2.0),
                egui::pos2(x, rect.center().y + height / 2.0),
            ],
            Stroke::new(2.0, p.accent),
        );
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}

pub fn nav_row(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let p = Palette::of(ui);
    let response = ui.add_sized(
        [ui.available_width(), 34.0],
        egui::Button::new("")
            .fill(if selected {
                p.selected
            } else {
                Color32::TRANSPARENT
            })
            .stroke(Stroke::NONE)
            .corner_radius(6),
    );
    if response.hovered() && !selected {
        ui.painter().rect_filled(response.rect, 6, p.hover);
    }
    let (text, inset) = if let Some(name) = text.strip_prefix("▫  ") {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                response.rect.left_center() + egui::vec2(12.0, 0.0),
                egui::vec2(6.0, 6.0),
            ),
            1,
            p.muted.gamma_multiply(0.5),
        );
        (name, 26.0)
    } else {
        (text, 10.0)
    };
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        FontId::proportional(14.0),
        p.text,
        (response.rect.width() - inset - 8.0).max(1.0),
    );
    job.wrap.max_rows = 1;
    let galley = ui.painter().layout_job(job);
    let origin = response.rect.left_center() + egui::vec2(inset, -galley.size().y / 2.0);
    ui.painter().galley(origin, galley, p.text);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), text));
    response
}

pub fn setting_row<R>(ui: &mut egui::Ui, label: &str, value: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        let result = ui
            .horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width() * 0.48, 32.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(egui::Label::new(label).wrap());
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), value)
                    .inner
            })
            .inner;
        ui.separator();
        result
    })
    .inner
}

pub fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    setting_row(ui, label, |ui| {
        let p = Palette::of(ui);
        let mut response = ui.add(
            egui::Button::new("")
                .min_size(egui::vec2(36.0, 28.0))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE),
        );
        if response.clicked() {
            *value = !*value;
            response.mark_changed();
        }
        let rect = egui::Rect::from_center_size(response.rect.center(), egui::vec2(30.0, 17.0));
        ui.painter()
            .rect_filled(rect, 9, if *value { p.text } else { p.border });
        ui.painter().circle_filled(
            rect.center() + egui::vec2(if *value { 6.5 } else { -6.5 }, 0.0),
            6.0,
            p.card,
        );
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *value, label)
        });
        response
    })
}

#[derive(Clone, Copy)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}
impl ToastLevel {
    pub fn color(self, palette: Palette) -> Color32 {
        match self {
            Self::Info | Self::Success | Self::Warning => palette.text,
            Self::Error => palette.red,
        }
    }
}

pub fn bar(p: Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(20)
        .inner_margin(12)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_font_covers_ui_text_and_symbols() {
        let ctx = egui::Context::default();
        install(&ctx);
        let output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                for font in [
                    FontId::proportional(16.0),
                    FontId::monospace(16.0),
                    FontId::new(16.0, FontFamily::Name("heading".into())),
                ] {
                    assert!(fonts.has_glyphs(
                        &font,
                        "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя +·…–«»"
                    ));
                }
                assert!(fonts.has_glyphs(&FontId::proportional(16.0), "?"));
            });
        });
        output.drop_without_applying_deltas();
    }
    #[test]
    fn system_tracks_os_and_explicit_theme_overrides_it() {
        let ctx = egui::Context::default();
        install(&ctx);
        for system in [egui::Theme::Light, egui::Theme::Dark] {
            let output = ctx.run_pass(
                egui::RawInput {
                    system_theme: Some(system),
                    ..Default::default()
                },
                |ctx| {
                    Theme::System.apply(ctx);
                    assert_eq!(ctx.theme(), system);
                    Theme::Light.apply(ctx);
                    assert!(!ctx.global_style().visuals.dark_mode);
                    Theme::Dark.apply(ctx);
                    assert!(ctx.global_style().visuals.dark_mode);
                },
            );
            output.drop_without_applying_deltas();
        }
    }
}
