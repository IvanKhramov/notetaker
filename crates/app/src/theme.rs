use eframe::egui::{self, Color32, FontData, FontFamily, FontId, Stroke, TextStyle};
use serde::{Deserialize, Serialize};
#[path = "icons.rs"]
pub mod icons;

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
    pub tertiary: Color32,
    pub border_strong: Color32,
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
                bg: rgb(27, 27, 26),
                tertiary: rgb(119, 118, 112),
                border_strong: rgb(61, 61, 58),
                sidebar: rgb(22, 22, 21),
                card: rgb(35, 35, 34),
                text: rgb(237, 237, 234),
                muted: rgb(163, 162, 156),
                border: rgb(46, 46, 44),
                soft: rgb(35, 35, 34),
                hover: rgb(42, 42, 40),
                selected: rgb(50, 50, 48),
                accent: rgb(95, 176, 108),
                on_accent: rgb(15, 36, 19),
                red: rgb(224, 103, 91),
                me: rgb(35, 35, 34),
            }
        } else {
            Self {
                bg: Color32::WHITE,
                tertiary: rgb(141, 140, 135),
                border_strong: rgb(220, 220, 216),
                sidebar: rgb(247, 247, 246),
                card: Color32::WHITE,
                text: rgb(28, 28, 26),
                muted: rgb(95, 94, 90),
                border: rgb(235, 235, 232),
                soft: rgb(247, 247, 246),
                hover: rgb(239, 239, 237),
                selected: rgb(232, 232, 229),
                accent: rgb(47, 122, 61),
                on_accent: Color32::WHITE,
                red: rgb(196, 69, 58),
                me: rgb(247, 247, 246),
            }
        }
    }
    pub fn of(ui: &egui::Ui) -> Self {
        Self::for_dark(ui.visuals().dark_mode)
    }
}

// Shared type scale, in logical points at 100% zoom.
pub const SMALL: f32 = 12.0;
/// Width kept free for a dialog's floating scroll bar: bar 5 + inner margin 3 + gap.
pub const SCROLL_LANE: f32 = 10.0;
pub const BUTTON: f32 = 14.0;
pub const BODY: f32 = 15.0;
pub const SUBHEADING: f32 = 16.0;
pub const SECTION: f32 = 16.0;
pub const HEADING: f32 = 24.0;
pub const TITLE: f32 = 26.0;

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("heading".into()))
}

/// Apply a settings selection once, then retain egui's native keyboard zoom.
/// Returns true when a keyboard change needs to be persisted.
pub fn sync_zoom(ctx: &egui::Context, applied: &mut Option<f32>, desired: &mut f32) -> bool {
    if applied.is_none_or(|last| last != *desired) {
        ctx.set_zoom_factor(*desired);
        *applied = Some(*desired);
    } else if (ctx.zoom_factor() - *desired).abs() > 0.001 {
        *desired = ctx.zoom_factor().clamp(0.5, 2.0);
        *applied = Some(*desired);
        ctx.set_zoom_factor(*desired);
        return true;
    }
    false
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
    for (name, bytes) in [
        (
            "medium",
            include_bytes!("../assets/Inter-Medium.ttf").as_slice(),
        ),
        ("icons", include_bytes!("../assets/lucide.ttf").as_slice()),
    ] {
        fonts.font_data.insert(
            name.into(),
            std::sync::Arc::new(FontData::from_static(bytes)),
        );
        fonts
            .families
            .insert(FontFamily::Name(name.into()), vec![name.into()]);
    }
    ctx.set_fonts(fonts);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        let p = Palette::for_dark(theme == egui::Theme::Dark);
        let mut style = egui::Style {
            animation_time: 0.12,
            ..Default::default()
        };
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 32.0;
        style.spacing.window_margin = egui::Margin::same(20);
        style.spacing.menu_margin = egui::Margin::same(8);
        style.spacing.extra_text_line_spacing = 3.0;
        style.spacing.scroll.bar_width = 5.0;
        style.spacing.scroll.floating_width = 3.0;
        style.spacing.scroll.bar_inner_margin = 3.0;
        // The handle uses the text color; keep it a quiet hint until hovered.
        style.spacing.scroll.dormant_handle_opacity = 0.25;
        style.spacing.scroll.active_handle_opacity = 0.35;
        style.spacing.scroll.interact_handle_opacity = 0.6;
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(BODY));
        style.text_styles.insert(TextStyle::Button, medium(BUTTON));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(SMALL));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(BUTTON));
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(HEADING, FontFamily::Name("heading".into())),
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
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(if v.dark_mode { 102 } else { 31 }),
        };
        v.hyperlink_color = p.text;
        v.text_cursor.stroke = Stroke::new(1.5, p.text);
        v.menu_corner_radius = 8.into();
        v.extreme_bg_color = p.card;
        v.faint_bg_color = p.sidebar;
        v.code_bg_color = p.soft;
        v.warn_fg_color = p.red;
        v.error_fg_color = p.red;
        // Accent tint: the neutral `selected` fill is invisible on white text areas.
        v.selection.bg_fill = if v.dark_mode {
            Color32::from_rgb(46, 78, 52)
        } else {
            Color32::from_rgb(208, 230, 212)
        };
        v.selection.stroke = Stroke::new(1.0, p.text);
        for widget in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            widget.corner_radius = 6.into();
            widget.bg_fill = p.soft;
            widget.weak_bg_fill = p.soft;
            widget.bg_stroke = Stroke::new(1.0, p.border_strong);
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

/// Shared document-style dialog header, with a quiet title and a close action.
pub struct Dialog<'a> {
    window: egui::Window<'a>,
    title: String,
    width: f32,
    anchored: bool,
    open: Option<&'a mut bool>,
}

pub fn modal<'a>(ctx: &egui::Context, title: impl Into<String>) -> Dialog<'a> {
    let bounds = ctx.content_rect().shrink(16.0);
    let title = title.into();
    Dialog {
        window: egui::Window::new(title.clone())
            .title_bar(false)
            .default_pos(bounds.center() - egui::vec2(280.0, 260.0))
            .max_width(bounds.width())
            .max_height(bounds.height())
            .constrain_to(bounds),
        title,
        width: 420.0,
        anchored: false,
        open: None,
    }
}

impl<'a> Dialog<'a> {
    pub fn open(mut self, open: &'a mut bool) -> Self {
        self.open = Some(open);
        self
    }
    pub fn default_width(mut self, width: f32) -> Self {
        self.width = width;
        self.window = self.window.default_width(width);
        self
    }
    pub fn default_size(mut self, size: [f32; 2]) -> Self {
        self.width = size[0];
        self.window = self.window.default_size(size);
        self
    }
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.window = self.window.resizable(resizable);
        self
    }
    #[cfg(any(windows, target_os = "macos"))]
    pub fn anchor(mut self, align: egui::Align2, offset: impl Into<egui::Vec2>) -> Self {
        self.anchored = true;
        self.window = self.window.anchor(align, offset);
        self
    }
    /// Returns true when the header close button was pressed.
    pub fn show(self, ctx: &egui::Context, contents: impl FnOnce(&mut egui::Ui)) -> bool {
        if self.open.as_deref() == Some(&false) {
            return false;
        }
        let mut close = false;
        let body = |ui: &mut egui::Ui| {
            ui.set_width(
                self.width
                    .min((ctx.content_rect().width() - 80.0).max(180.0)),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&self.title).font(semibold(SUBHEADING)))
                        .truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = icon_button(ui, icons::X, "Закрыть").clicked();
                });
            });
            ui.add_space(4.0);
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .id_salt("dialog_body")
                .auto_shrink([false, true])
                .max_height((ctx.content_rect().height() - 160.0).max(80.0))
                .show(ui, contents);
        };
        if self.anchored {
            self.window.resizable(false).movable(false).show(ctx, body);
        } else {
            let dark = ctx.global_style().visuals.dark_mode;
            let response = egui::Modal::new(egui::Id::unique(("modal", &self.title)))
                .backdrop_color(Color32::from_black_alpha(if dark { 140 } else { 89 }))
                .frame(
                    egui::Frame::window(&ctx.global_style()).shadow(egui::epaint::Shadow {
                        offset: [0, 16],
                        blur: 48,
                        spread: 0,
                        color: Color32::from_black_alpha(if dark { 102 } else { 51 }),
                    }),
                )
                .show(ctx, body);
            close |= response.should_close();
        }
        // Esc closes the dialog unless a menu or combo box is open and takes it first.
        close |= !egui::Popup::is_any_open(ctx)
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if close && let Some(open) = self.open {
            *open = false;
        }
        close
    }
}

#[cfg(any(windows, target_os = "macos"))]
#[derive(Default)]
pub enum UpdateAction {
    #[default]
    None,
    Install,
    Notes,
    Later,
}

#[cfg(any(windows, target_os = "macos"))]
pub fn update_prompt(
    ctx: &egui::Context,
    title: String,
    message: &str,
    install_label: &str,
    can_install: bool,
    can_dismiss: bool,
    show_notes: bool,
) -> UpdateAction {
    let mut action = UpdateAction::None;
    let close = modal(ctx, title)
        .default_width(420.0)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -20.0])
        .show(ctx, |ui| {
            if !message.is_empty() {
                ui.label(message);
                ui.add_space(8.0);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can_install,
                        styled_button(Palette::of(ui), install_label, ButtonStyle::Primary),
                    )
                    .clicked()
                {
                    action = UpdateAction::Install;
                }
                if show_notes && button(ui, "Что нового", ButtonStyle::Ghost).clicked() {
                    action = UpdateAction::Notes;
                }
                if ui
                    .add_enabled(
                        can_dismiss,
                        styled_button(Palette::of(ui), "Позже", ButtonStyle::Ghost),
                    )
                    .clicked()
                {
                    action = UpdateAction::Later;
                }
            });
        });
    if close && can_dismiss {
        action = UpdateAction::Later;
    }
    action
}

pub enum ButtonStyle {
    Primary,
    Secondary,
    Ghost,
    Stop,
    Danger,
}
pub fn button(ui: &mut egui::Ui, text: &str, style: ButtonStyle) -> egui::Response {
    let response = ui.add(styled_button(Palette::of(ui), text, style));
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            6,
            Stroke::new(2.0, Palette::of(ui).accent),
            egui::StrokeKind::Outside,
        );
    }
    response
}
pub fn styled_button(p: Palette, text: &str, style: ButtonStyle) -> egui::Button<'_> {
    let (fill, ink, border) = match style {
        ButtonStyle::Danger => (p.red, Color32::WHITE, Stroke::NONE),
        ButtonStyle::Stop | ButtonStyle::Secondary => (
            Color32::TRANSPARENT,
            p.text,
            Stroke::new(1.0, p.border_strong),
        ),
        ButtonStyle::Primary => (p.accent, p.on_accent, Stroke::NONE),
        ButtonStyle::Ghost => (Color32::TRANSPARENT, p.muted, Stroke::NONE),
    };
    let mut job = egui::text::LayoutJob::default();
    if matches!(
        text,
        "Новая запись"
            | "Стоп"
            | "Копировать"
            | "Копировать всё"
            | "Удалить"
            | "Переименовать"
    ) {
        job.append(
            icon_for_label(text),
            0.0,
            egui::TextFormat {
                font_id: FontId::new(16.0, FontFamily::Name("icons".into())),
                color: ink,
                ..Default::default()
            },
        );
        job.append(
            " ",
            6.0,
            egui::TextFormat {
                font_id: medium(BUTTON),
                color: ink,
                ..Default::default()
            },
        );
    }
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: medium(BUTTON),
            color: ink,
            ..Default::default()
        },
    );
    egui::Button::new(job)
        .fill(fill)
        .stroke(border)
        .corner_radius(6)
}

/// A text action inside prose. Always underlined: links use the text color and would
/// otherwise read as a plain sentence until hovered.
pub fn action_link(ui: &mut egui::Ui, text: &str) -> egui::Response {
    button(ui, text, ButtonStyle::Ghost)
}

pub fn section_caption(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(SMALL)
            .color(Palette::of(ui).muted),
    );
}
pub fn page_heading(ui: &mut egui::Ui, text: &str) {
    ui.heading(text);
    ui.add_space(24.0);
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
            egui::Label::new(egui::RichText::new(text).size(SMALL).color(p.muted)),
        );
    });
}
pub fn segmented<T: Copy + PartialEq>(ui: &mut egui::Ui, selected: &mut T, values: &[(T, &str)]) {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.soft)
        .corner_radius(6)
        .inner_margin(4)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for &(value, text) in values {
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new(text).font(medium(BUTTON)))
                                .min_size(egui::vec2(0.0, 24.0))
                                .corner_radius(4)
                                .stroke(Stroke::NONE)
                                .fill(if *selected == value {
                                    p.card
                                } else {
                                    Color32::TRANSPARENT
                                }),
                        )
                        .clicked()
                    {
                        *selected = value;
                    }
                }
            });
        });
}
/// Animated recording state. It is the only colored ornament in navigation.
/// Recording indicator. Its bars follow the captured level, so a silent input shows as flat.
pub fn waveform(ui: &mut egui::Ui, level: f32) {
    let p = Palette::of(ui);
    // Perceptual scale: speech peaks around 0.1..0.5 fill most of the height.
    let amplitude = ui.ctx().animate_value_with_time(
        ui.scope_id().with("waveform"),
        level.clamp(0.0, 1.0).sqrt(),
        0.12,
    );
    let (rect, _) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
    let time = ui.input(|i| i.time) as f32;
    for n in 0..5 {
        let height = 3.0 + 14.0 * amplitude * (time * 3.0 + n as f32 * 1.2).sin().abs();
        let x = rect.left() + 3.0 + n as f32 * 4.0;
        ui.painter().line_segment(
            [
                egui::pos2(x, rect.center().y - height / 2.0),
                egui::pos2(x, rect.center().y + height / 2.0),
            ],
            Stroke::new(2.0, p.red),
        );
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}

pub fn nav_row(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let response = ui.add_sized(
        [ui.available_width(), 30.0],
        nav_button(Palette::of(ui), selected),
    );
    nav_label(ui, response, text, selected)
}
/// The empty frame of a navigation row; `nav_label` paints its text.
pub fn nav_button(p: Palette, selected: bool) -> egui::Button<'static> {
    egui::Button::new("")
        .fill(if selected {
            p.selected
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::NONE)
        .corner_radius(6)
}
pub fn nav_label(
    ui: &mut egui::Ui,
    response: egui::Response,
    text: &str,
    selected: bool,
) -> egui::Response {
    let p = Palette::of(ui);
    if response.hovered() && !selected {
        ui.painter().rect_filled(response.rect, 6, p.hover);
    }
    let (text, glyph) = if let Some(name) = text.strip_prefix("▫  ") {
        (name, icons::FOLDER)
    } else {
        (text, icon_for_label(text))
    };
    paint_icon(
        ui,
        response.rect.left_center() + egui::vec2(16.0, 0.0),
        glyph,
        16.0,
        if response.hovered() || selected {
            p.text
        } else {
            p.muted
        },
    );
    let inset = 30.0;
    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        if selected {
            medium(BUTTON)
        } else {
            FontId::proportional(BUTTON)
        },
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
        // The divider precedes the row, so a hint drawn right after a row stays in its group.
        ui.separator();
        ui.spacing_mut().item_spacing.y = 3.0;
        if ui.available_width() < 440.0 {
            ui.label(label);
            ui.horizontal(value).inner
        } else {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width() * 0.48, 44.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.add(egui::Label::new(label).wrap());
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), value)
                    .inner
            })
            .inner
        }
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
        let rect = egui::Rect::from_center_size(response.rect.center(), egui::vec2(34.0, 20.0));
        ui.painter()
            .rect_filled(rect, 9, if *value { p.accent } else { p.border_strong });
        ui.painter().circle_filled(
            rect.center()
                + egui::vec2(
                    -7.0 + 14.0 * ui.ctx().animate_bool_with_time(response.id, *value, 0.16),
                    0.0,
                ),
            8.0,
            Color32::WHITE,
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
        .corner_radius(12)
        .inner_margin(12)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_and_native_zoom_shortcuts_share_the_persisted_scale() {
        let ctx = egui::Context::default();
        let mut applied = None;
        let mut desired = 1.25;
        let frame = |ctx: &egui::Context| {
            ctx.run_ui(Default::default(), |_| {})
                .drop_without_applying_deltas();
        };
        assert!(!sync_zoom(&ctx, &mut applied, &mut desired));
        frame(&ctx);
        assert_eq!(ctx.zoom_factor(), 1.25);
        for (key, expected) in [
            (egui::Key::Equals, 1.4),
            (egui::Key::Minus, 1.3),
            (egui::Key::Num0, 1.0),
        ] {
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::COMMAND,
                    }],
                    ..Default::default()
                },
                |_| {},
            )
            .drop_without_applying_deltas();
            frame(&ctx);
            assert!(sync_zoom(&ctx, &mut applied, &mut desired));
            assert!((desired - expected).abs() < 0.001);
            assert!(!sync_zoom(&ctx, &mut applied, &mut desired));
        }
    }
    #[test]
    fn embedded_font_covers_ui_text_and_symbols() {
        let ctx = egui::Context::default();
        install(&ctx);
        let output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                for font in [
                    FontId::proportional(BODY),
                    FontId::monospace(BODY),
                    FontId::new(BODY, FontFamily::Name("heading".into())),
                ] {
                    assert!(fonts.has_glyphs(
                        &font,
                        "АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя +·…–«»"
                    ));
                }
                assert!(fonts.has_glyphs(&FontId::proportional(BODY), "?"));
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

pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("medium".into()))
}
pub fn paint_icon(ui: &egui::Ui, center: egui::Pos2, glyph: &str, size: f32, color: Color32) {
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        glyph,
        FontId::new(size, FontFamily::Name("icons".into())),
        color,
    );
}
pub fn icon_for_label(label: &str) -> &'static str {
    match label {
        "Встречи" | "Календари" => icons::CALENDAR,
        "Чат" => icons::MESSAGE_CIRCLE,
        "Настройки" | "Общие" => icons::SETTINGS,
        "Запись" => icons::MIC,
        "Поиск" => icons::SEARCH,
        "AI" => icons::SPARKLES,
        "Хранилище" => icons::HARD_DRIVE,
        "Ещё" => icons::ELLIPSIS,
        "Люди" | "Участники" => icons::USERS,
        "Удалить" => icons::TRASH_2,
        "Переименовать" => icons::PENCIL,
        "Копировать" | "Копировать всё" => icons::COPY,
        "Пересоздать конспект" => icons::REFRESH_CW,
        "Задачи" => icons::CHECK,
        "Экспорт" => icons::DOWNLOAD,
        "Диагностика" => icons::INFO,
        "Ошибка" => icons::TRIANGLE_ALERT,
        "База" => icons::DATABASE,
        "Новая запись" => icons::MIC,
        "Стоп" => icons::SQUARE,
        "Назад" => icons::CHEVRON_LEFT,
        "Добавить" => icons::PLUS,
        _ => icons::FILE_TEXT,
    }
}
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let p = Palette::of(ui);
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    if t > 0.0 {
        ui.painter().rect_filled(rect, 6, p.hover.gamma_multiply(t));
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(1.0),
            6,
            Stroke::new(2.0, p.accent),
            egui::StrokeKind::Outside,
        );
    }
    paint_icon(
        ui,
        rect.center(),
        glyph,
        16.0,
        if !ui.is_enabled() {
            p.tertiary
        } else if response.hovered() {
            p.text
        } else {
            p.muted
        },
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), tooltip));
    response.on_hover_text(tooltip)
}
pub fn select_icon(ui: &egui::Ui, rect: egui::Rect, _: &egui::style::WidgetVisuals, _: bool) {
    paint_icon(
        ui,
        rect.center(),
        icons::CHEVRON_DOWN,
        16.0,
        Palette::of(ui).muted,
    );
}
pub fn disclosure(title: impl Into<egui::WidgetText>) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(title).icon(|ui, openness, response| {
        let center = response.rect.center();
        let angle = openness * std::f32::consts::FRAC_PI_2;
        let rot = egui::emath::Rot2::from_angle(angle);
        let points = [
            egui::vec2(-2.0, -4.0),
            egui::vec2(2.0, 0.0),
            egui::vec2(-2.0, 4.0),
        ]
        .map(|v| center + rot * v);
        ui.painter().add(egui::Shape::line(
            points.to_vec(),
            Stroke::new(1.5, Palette::of(ui).muted),
        ));
    })
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    fn luminance(color: Color32) -> f32 {
        let c = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * c(color.r()) + 0.7152 * c(color.g()) + 0.0722 * c(color.b())
    }
    fn contrast(a: Color32, b: Color32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }
    #[test]
    fn palette_text_contrast() {
        for dark in [false, true] {
            let p = Palette::for_dark(dark);
            for bg in [p.bg, p.sidebar] {
                assert!(contrast(p.muted, bg) >= 4.5);
                assert!(contrast(p.tertiary, bg) >= 3.0);
            }
            assert!(contrast(p.accent, p.on_accent) >= 4.5);
        }
    }
    #[test]
    fn embedded_lucide_glyph_coverage() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.run_pass(Default::default(), |ctx| {
            ctx.fonts_mut(|fonts| {
                for glyph in icons::ALL {
                    assert!(
                        fonts.has_glyphs(
                            &FontId::new(16.0, FontFamily::Name("icons".into())),
                            glyph
                        ),
                        "Missing icon {glyph}"
                    );
                }
            });
        })
        .drop_without_applying_deltas();
    }
}

/// Popup choice with the same value semantics as egui's selectable_value.
pub fn select_value<T: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    value: T,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    let selected = *current == value;
    let mut button = egui::Button::new(label.into())
        .selected(selected)
        .min_size(egui::vec2(ui.available_width(), 30.0))
        .corner_radius(6)
        .frame(false);
    if selected {
        button = button.right_text(
            egui::RichText::new(icons::CHECK)
                .font(FontId::new(16.0, FontFamily::Name("icons".into()))),
        );
    }
    let mut response = ui.add(button);
    if response.clicked() && !selected {
        *current = value;
        response.mark_changed();
    }
    response
}
