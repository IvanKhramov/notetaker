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
    pub rail: Color32,
    pub caption: Color32,
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
    pub danger: Color32,
    pub me: Color32,
}
impl Palette {
    pub fn for_dark(dark: bool) -> Self {
        let rgb = Color32::from_rgb;
        if dark {
            Self {
                bg: rgb(23, 22, 26),
                sidebar: rgb(26, 24, 31),
                rail: rgb(15, 14, 18),
                card: rgb(31, 29, 37),
                text: rgb(233, 230, 239),
                muted: rgb(141, 135, 152),
                caption: rgb(111, 106, 120),
                border: rgb(38, 35, 44),
                soft: rgb(34, 31, 41),
                hover: rgb(34, 31, 41),
                selected: rgb(42, 38, 51),
                accent: rgb(201, 184, 255),
                on_accent: rgb(26, 21, 48),
                red: rgb(255, 77, 94),
                danger: rgb(56, 28, 37),
                me: rgb(42, 38, 51),
            }
        } else {
            Self {
                bg: rgb(247, 246, 250),
                sidebar: rgb(242, 240, 247),
                rail: rgb(239, 237, 244),
                card: Color32::WHITE,
                text: rgb(37, 31, 51),
                muted: rgb(115, 106, 131),
                caption: rgb(133, 123, 148),
                border: rgb(226, 221, 234),
                soft: rgb(239, 236, 246),
                hover: rgb(232, 227, 241),
                selected: rgb(230, 222, 248),
                accent: rgb(107, 79, 216),
                on_accent: Color32::WHITE,
                red: rgb(255, 77, 94),
                danger: rgb(255, 232, 237),
                me: rgb(239, 233, 251),
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
        v.hyperlink_color = p.accent;
        v.text_cursor.stroke = Stroke::new(1.5, p.accent);
        v.menu_corner_radius = 8.into();
        v.extreme_bg_color = p.card;
        v.faint_bg_color = p.sidebar;
        v.code_bg_color = p.soft;
        v.warn_fg_color = p.red;
        v.error_fg_color = p.red;
        v.selection.bg_fill = p.selected;
        v.selection.stroke = Stroke::new(1.0, p.accent);
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
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.hovered.bg_fill = p.hover;
        v.widgets.hovered.weak_bg_fill = p.hover;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent.gamma_multiply(0.5));
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
    Danger,
}
pub fn button(ui: &mut egui::Ui, text: &str, style: ButtonStyle) -> egui::Response {
    ui.add(styled_button(Palette::of(ui), text, style))
}
pub fn styled_button(p: Palette, text: &str, style: ButtonStyle) -> egui::Button<'_> {
    let (fill, ink, border) = match style {
        ButtonStyle::Primary => (p.accent, p.on_accent, Stroke::NONE),
        ButtonStyle::Secondary => (p.soft, p.text, Stroke::new(1.0, p.border)),
        ButtonStyle::Ghost => (Color32::TRANSPARENT, p.muted, Stroke::NONE),
        ButtonStyle::Danger => (p.danger, p.red, Stroke::NONE),
    };
    egui::Button::new(egui::RichText::new(text).color(ink))
        .fill(fill)
        .stroke(border)
        .corner_radius(8)
}

#[derive(Clone, Copy)]
pub enum Icon {
    Help,
    Settings,
    Reload,
    Meetings,
    Tasks,
    People,
    Series,
    Digest,
    Search,
    Import,
}
fn paint_icon(ui: &egui::Ui, rect: egui::Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let painter = ui.painter();
    let stroke = Stroke::new(1.5, color);
    let line = |a: [f32; 2], b: [f32; 2]| {
        painter.line_segment(
            [c + egui::vec2(a[0], a[1]), c + egui::vec2(b[0], b[1])],
            stroke,
        );
    };
    match icon {
        Icon::Help => {
            painter.circle_stroke(c, 8.0, stroke);
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                "?",
                FontId::proportional(14.0),
                color,
            );
        }
        Icon::Settings => {
            painter.circle_stroke(c, 5.5, stroke);
            painter.circle_stroke(c, 2.0, stroke);
            for n in 0..8 {
                let direction = egui::Vec2::angled(n as f32 * std::f32::consts::TAU / 8.0);
                painter.line_segment([c + direction * 5.5, c + direction * 8.5], stroke);
            }
        }
        Icon::Reload => {
            let points = (0..=24)
                .map(|n| {
                    let angle = -std::f32::consts::FRAC_PI_2
                        + std::f32::consts::TAU * (0.2 + n as f32 * 0.8 / 24.0);
                    c + egui::Vec2::angled(angle) * 7.0
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
            line([-2.0, -10.0], [3.0, -7.0]);
            line([-2.0, -4.0], [3.0, -7.0]);
        }
        Icon::Meetings | Icon::Digest => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(14.0, 17.0)),
                2,
                stroke,
                egui::StrokeKind::Inside,
            );
            for y in [-4.0, 0.0, 4.0] {
                line(
                    [-3.0, y],
                    [
                        if matches!(icon, Icon::Digest) && y == 4.0 {
                            0.0
                        } else {
                            3.0
                        },
                        y,
                    ],
                );
            }
        }
        Icon::Tasks => {
            painter.rect_stroke(
                egui::Rect::from_center_size(c, egui::vec2(16.0, 16.0)),
                4,
                stroke,
                egui::StrokeKind::Inside,
            );
            line([-4.0, 0.0], [-1.0, 3.0]);
            line([-1.0, 3.0], [4.0, -3.0]);
        }
        Icon::People => {
            painter.circle_stroke(c + egui::vec2(-3.0, -4.0), 3.5, stroke);
            painter.circle_stroke(c + egui::vec2(5.0, -2.0), 2.5, stroke);
            painter.add(egui::Shape::line(
                vec![
                    c + egui::vec2(-9.0, 7.0),
                    c + egui::vec2(-8.0, 3.0),
                    c + egui::vec2(-3.0, 1.0),
                    c + egui::vec2(2.0, 3.0),
                    c + egui::vec2(3.0, 7.0),
                ],
                stroke,
            ));
            line([5.0, 2.0], [8.0, 4.0]);
            line([8.0, 4.0], [9.0, 7.0]);
        }
        Icon::Series => {
            for y in [-5.0, 5.0] {
                painter.rect_stroke(
                    egui::Rect::from_center_size(c + egui::vec2(0.0, y), egui::vec2(16.0, 6.0)),
                    2,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
        Icon::Search => {
            painter.circle_stroke(c + egui::vec2(-2.0, -2.0), 6.0, stroke);
            line([2.5, 2.5], [8.0, 8.0]);
        }
        Icon::Import => {
            line([-8.0, 3.0], [-8.0, 8.0]);
            line([-8.0, 8.0], [8.0, 8.0]);
            line([8.0, 8.0], [8.0, 3.0]);
            line([0.0, -9.0], [0.0, 3.0]);
            line([-4.0, -1.0], [0.0, 3.0]);
            line([0.0, 3.0], [4.0, -1.0]);
        }
    }
}
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    icon_control(ui, icon, tooltip, false, 28.0)
}
pub fn rail_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str, selected: bool) -> egui::Response {
    icon_control(ui, icon, tooltip, selected, 40.0)
}
fn icon_control(
    ui: &mut egui::Ui,
    icon: Icon,
    tooltip: &str,
    selected: bool,
    size: f32,
) -> egui::Response {
    let p = Palette::of(ui);
    let response = ui
        .add(
            egui::Button::new("")
                .min_size(egui::vec2(size, size))
                .fill(if selected {
                    p.selected
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(Stroke::NONE)
                .corner_radius(10),
        )
        .on_hover_text(tooltip);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tooltip));
    if response.hovered() && !selected {
        ui.painter().rect_filled(response.rect, 10, p.hover);
    }
    paint_icon(
        ui,
        response.rect,
        icon,
        if selected {
            p.accent
        } else if response.hovered() {
            p.text
        } else {
            p.muted
        },
    );
    response
}
pub fn record_button(ui: &mut egui::Ui, recording: bool) -> egui::Response {
    let p = Palette::of(ui);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::click());
    let response = response.on_hover_text(if recording {
        "Стоп"
    } else {
        "+ Новая заметка"
    });
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::Role::Button,
            ui.is_enabled(),
            if recording {
                "Стоп"
            } else {
                "+ Новая заметка"
            },
        )
    });
    let pulse = if recording {
        0.7 + 0.3 * ui.input(|i| (i.time * 3.0).sin() as f32)
    } else {
        1.0
    };
    ui.painter()
        .circle_filled(rect.center(), 20.0, p.red.gamma_multiply(0.20 * pulse));
    ui.painter().circle_filled(
        rect.center(),
        15.0,
        p.red
            .gamma_multiply(if ui.is_enabled() { 1.0 } else { 0.35 }),
    );
    if recording {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(rect.center(), egui::vec2(10.0, 10.0)),
            2,
            Color32::WHITE,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    } else {
        ui.painter()
            .circle_filled(rect.center(), 4.0, Color32::WHITE);
    }
    response
}
pub fn folder_color(name: Option<&str>, p: Palette) -> Color32 {
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return p.caption;
    };
    const COLORS: [Color32; 6] = [
        Color32::from_rgb(167, 145, 225),
        Color32::from_rgb(105, 174, 159),
        Color32::from_rgb(217, 159, 113),
        Color32::from_rgb(114, 158, 210),
        Color32::from_rgb(209, 133, 159),
        Color32::from_rgb(176, 174, 113),
    ];
    let hash = name
        .bytes()
        .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    COLORS[hash % COLORS.len()]
}
pub fn section_caption(ui: &mut egui::Ui, text: &str) {
    let format = egui::TextFormat {
        font_id: FontId::new(10.5, FontFamily::Name("heading".into())),
        color: Palette::of(ui).caption,
        extra_letter_spacing: 1.2,
        ..Default::default()
    };
    ui.label(egui::text::LayoutJob::single_section(
        text.to_uppercase(),
        format,
    ));
}
pub fn page_heading(ui: &mut egui::Ui, text: &str) {
    section_caption(ui, "notetaker");
    ui.add_space(6.0);
    ui.heading(text);
    ui.add_space(12.0);
}
pub fn card(p: Palette, selected: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(if selected { p.selected } else { p.card })
        .stroke(Stroke::new(
            1.0,
            if selected {
                p.accent.gamma_multiply(0.35)
            } else {
                p.border
            },
        ))
        .corner_radius(10)
        .inner_margin(12)
}
pub fn chip(ui: &mut egui::Ui, text: &str) {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.soft)
        .corner_radius(20)
        .inner_margin(egui::Margin::symmetric(9, 4))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(11.5).color(p.muted));
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
                    p.accent
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
#[derive(Clone, Copy)]
pub enum TimelineDot {
    Upcoming,
    Past(Color32),
    Live,
}
pub fn timeline_row<R>(
    ui: &mut egui::Ui,
    dot: TimelineDot,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let p = Palette::of(ui);
    let top = ui.cursor().top();
    let x = ui.max_rect().left() + 5.0;
    let painter = ui.painter().clone();
    // Reserve the line behind the row, so opaque cards always paint above it.
    let line = painter.add(egui::Shape::Noop);
    let response = ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_max(
            egui::pos2(x + 15.0, top),
            ui.max_rect().max,
        )),
        content,
    );
    let bottom = response.response.rect.bottom() + ui.spacing().item_spacing.y;
    painter.set(
        line,
        egui::Shape::line_segment(
            [egui::pos2(x, top), egui::pos2(x, bottom)],
            Stroke::new(2.0, p.border),
        ),
    );
    let center = egui::pos2(x, top + (response.response.rect.height() * 0.5).min(21.0));
    match dot {
        TimelineDot::Upcoming => {
            painter.circle_filled(center, 5.0, p.sidebar);
            painter.circle_stroke(center, 4.0, Stroke::new(1.5, p.accent));
        }
        TimelineDot::Past(color) => {
            painter.circle_filled(center, 3.5, color);
        }
        TimelineDot::Live => {
            painter.circle_filled(center, 8.0, p.red.gamma_multiply(0.2));
            painter.circle_filled(center, 4.0, p.red);
        }
    }
    response
}
pub fn now_divider(ui: &mut egui::Ui) {
    let p = Palette::of(ui);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 28.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(egui::pos2(rect.left() + 5.0, rect.center().y), 3.0, p.red);
    ui.painter().text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "СЕЙЧАС",
        FontId::new(9.5, FontFamily::Name("heading".into())),
        p.red,
    );
    ui.painter().line_segment(
        [
            egui::pos2(rect.left() + 78.0, rect.center().y),
            rect.right_center(),
        ],
        Stroke::new(1.0, p.red.gamma_multiply(0.4)),
    );
}
pub fn recording_dot(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
    let p = Palette::of(ui);
    ui.painter()
        .circle_filled(rect.center(), 8.0, p.red.gamma_multiply(0.2));
    ui.painter().circle_filled(rect.center(), 3.5, p.red);
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
    card(p, false)
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
