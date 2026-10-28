use eframe::egui::{self, Color32, FontData, FontFamily, FontId, Stroke, TextStyle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    #[default]
    System,
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
    pub card: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub border: Color32,
    pub soft: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub red: Color32,
    pub danger: Color32,
    pub me: Color32,
}
impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        let rgb = Color32::from_rgb;
        if dark {
            Self {
                bg: rgb(35, 33, 30),
                sidebar: rgb(29, 27, 25),
                card: rgb(46, 43, 39),
                text: rgb(239, 235, 227),
                muted: rgb(164, 157, 145),
                border: rgb(65, 60, 53),
                soft: rgb(55, 51, 45),
                accent: rgb(239, 235, 227),
                on_accent: rgb(31, 29, 26),
                red: rgb(245, 103, 107),
                danger: rgb(78, 39, 40),
                me: rgb(39, 54, 81),
            }
        } else {
            Self {
                bg: rgb(251, 250, 248),
                sidebar: rgb(243, 241, 236),
                card: Color32::WHITE,
                text: rgb(31, 29, 26),
                muted: rgb(155, 150, 140),
                border: rgb(231, 227, 220),
                soft: rgb(239, 236, 230),
                accent: rgb(31, 29, 26),
                on_accent: Color32::WHITE,
                red: rgb(229, 72, 77),
                danger: rgb(253, 232, 232),
                me: rgb(232, 238, 252),
            }
        }
    }
    pub fn of(ui: &egui::Ui) -> Self {
        Self::for_dark(ui.visuals().dark_mode)
    }
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
        style.spacing.item_spacing = egui::vec2(8.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 30.0;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(29.0, FontFamily::Name("heading".into())),
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
        v.menu_corner_radius = 8.into();
        v.extreme_bg_color = p.card;
        v.faint_bg_color = p.sidebar;
        v.code_bg_color = p.soft;
        v.warn_fg_color = p.red;
        v.error_fg_color = p.red;
        v.selection.bg_fill = p.me;
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
        v.widgets.hovered.bg_fill = p.border;
        v.widgets.active.bg_fill = p.me;
        ctx.set_style_of(theme, style);
    }
}

pub enum ButtonStyle {
    Primary,
    Secondary,
    Danger,
}
pub fn button(ui: &mut egui::Ui, text: &str, style: ButtonStyle) -> egui::Response {
    let p = Palette::of(ui);
    let (fill, ink) = match style {
        ButtonStyle::Primary => (p.accent, p.on_accent),
        ButtonStyle::Secondary => (p.soft, p.text),
        ButtonStyle::Danger => (p.danger, p.red),
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(ink))
            .fill(fill)
            .stroke(Stroke::NONE),
    )
}
pub enum Icon {
    Help,
    Settings,
    Reload,
}
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    let response = ui
        .scope(|ui| {
            ui.spacing_mut().button_padding = egui::vec2(4.0, 2.0);
            ui.spacing_mut().interact_size = egui::vec2(24.0, 24.0);
            ui.add(
                egui::Button::new("")
                    .frame(false)
                    .min_size(egui::vec2(24.0, 24.0)),
            )
        })
        .inner
        .on_hover_text(tooltip);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tooltip));
    let center = response.rect.center();
    let stroke = Stroke::new(1.4, Palette::of(ui).muted);
    let painter = ui.painter();
    match icon {
        Icon::Help => {
            painter.text(
                center,
                egui::Align2::CENTER_CENTER,
                "?",
                FontId::proportional(16.0),
                stroke.color,
            );
        }
        Icon::Settings => {
            painter.circle_stroke(center, 4.5, stroke);
            painter.circle_stroke(center, 1.5, stroke);
            for n in 0..8 {
                let direction = egui::Vec2::angled(n as f32 * std::f32::consts::TAU / 8.0);
                painter.line_segment([center + direction * 4.5, center + direction * 7.0], stroke);
            }
        }
        Icon::Reload => {
            let points = (0..=24)
                .map(|n| {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * (0.2 + n as f32 * 0.8 / 24.0);
                    center + egui::Vec2::angled(angle) * 6.0
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + egui::vec2(-1.0, -9.0),
                    center + egui::vec2(-1.0, -3.0),
                    center + egui::vec2(4.0, -6.0),
                ],
                stroke.color,
                Stroke::NONE,
            ));
        }
    }
    response
}
pub fn nav_item(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let p = Palette::of(ui);
    ui.scope(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(8.0, 4.0);
        ui.spacing_mut().interact_size.y = 28.0;
        ui.visuals_mut().selection.bg_fill = p.card;
        ui.add_sized(
            [ui.available_width(), 28.0],
            egui::Button::new(())
                .left_text(egui::RichText::new(text).color(p.text))
                .selected(selected)
                .frame_when_inactive(selected)
                .frame(true)
                .stroke(Stroke::NONE)
                .truncate(),
        )
    })
    .inner
}
pub fn chip(ui: &mut egui::Ui, text: &str) {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.soft)
        .corner_radius(20)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(p.muted));
        });
}
pub fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, selected: &mut T, values: &[(T, &str)]) {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.soft)
        .corner_radius(9)
        .inner_margin(3)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for &(value, text) in values {
                    let active = *selected == value;
                    let text =
                        egui::RichText::new(text).color(if active { p.text } else { p.muted });
                    if ui
                        .add(
                            egui::Button::new(text)
                                .fill(if active { p.card } else { Color32::TRANSPARENT })
                                .stroke(Stroke::NONE),
                        )
                        .clicked()
                    {
                        *selected = value;
                    }
                }
            });
        });
}
pub fn recording_dot(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 16.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), 4.0, Palette::of(ui).red);
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
            Self::Info => palette.text,
            Self::Success => {
                if palette.bg.r() < 128 {
                    Color32::from_rgb(126, 201, 150)
                } else {
                    Color32::from_rgb(38, 116, 64)
                }
            }
            Self::Warning => {
                if palette.bg.r() < 128 {
                    Color32::from_rgb(225, 189, 106)
                } else {
                    Color32::from_rgb(145, 103, 26)
                }
            }
            Self::Error => palette.red,
        }
    }
}

pub fn bar(p: Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(10)
        .inner_margin(10)
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
