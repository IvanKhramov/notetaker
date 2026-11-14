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
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub red: Color32,
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
                accent_hover: rgb(81, 158, 94),
                on_accent: rgb(15, 36, 19),
                red: rgb(224, 103, 91),
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
                accent_hover: rgb(40, 106, 53),
                on_accent: Color32::WHITE,
                red: rgb(196, 69, 58),
            }
        }
    }
    pub fn of(ui: &egui::Ui) -> Self {
        Self::for_dark(ui.visuals().dark_mode)
    }
}

pub const SPACE: [f32; 6] = [4.0, 8.0, 12.0, 16.0, 24.0, 32.0];
pub const RADIUS_CONTROL: u8 = 6;
pub const RADIUS_SURFACE: u8 = 8;
pub const RADIUS_MODAL: u8 = 12;
pub const CONTROL_HEIGHT: f32 = 32.0;
pub const COMPACT_HEIGHT: f32 = 28.0;
pub const CTA_HEIGHT: f32 = 36.0;

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
    // egui clears focus on Esc before UI code runs. Keep the text edit that
    // owned focus at the previous pass end, scoped to each viewport.
    ctx.on_begin_pass(
        "notetaker-escape-focus",
        std::sync::Arc::new(|ctx| {
            if ctx.current_pass_index() == 0 {
                let id = escape_focus_id(ctx);
                let previous = ctx
                    .data(|data| data.get_temp::<EscapeFocus>(id))
                    .unwrap_or_default();
                let at_start = previous
                    .last_text_edit
                    .is_some_and(|id| ctx.memory(|mem| mem.had_focus_last_frame(id)));
                ctx.data_mut(|data| {
                    data.insert_temp(
                        id,
                        EscapeFocus {
                            at_start,
                            ..previous
                        },
                    )
                });
            }
        }),
    );
    ctx.on_end_pass(
        "notetaker-escape-focus",
        std::sync::Arc::new(|ctx| {
            let last_text_edit = ctx
                .memory(|mem| mem.focused())
                .filter(|id| egui::text_edit::TextEditState::load(ctx, *id).is_some());
            let id = escape_focus_id(ctx);
            ctx.data_mut(|data| {
                let mut focus = data.get_temp::<EscapeFocus>(id).unwrap_or_default();
                focus.last_text_edit = last_text_edit;
                data.insert_temp(id, focus);
            });
        }),
    );
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
        v.text_edit_bg_color = Some(p.bg);
        v.indent_has_left_vline = false;
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
        v.selection.stroke = Stroke::new(1.0, p.accent);
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

#[derive(Clone, Copy, Default)]
struct EscapeFocus {
    last_text_edit: Option<egui::Id>,
    at_start: bool,
}

fn escape_focus_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::unique(("escape-text-focus", ctx.viewport_id()))
}

pub fn escape_to_close(ctx: &egui::Context) -> bool {
    let text_edit_had_focus = ctx
        .data(|data| data.get_temp::<EscapeFocus>(escape_focus_id(ctx)))
        .is_some_and(|focus| focus.at_start);
    !text_edit_had_focus
        && !egui::Popup::is_any_open(ctx)
        && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}

pub fn modal_should_close<T>(response: &egui::ModalResponse<T>) -> bool {
    response.backdrop_response.clicked()
        || response.response.should_close()
        || (response.is_top_modal
            && !response.any_popup_open
            && escape_to_close(&response.response.ctx))
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
        self.show_inner(ctx, contents, None::<fn(&mut egui::Ui)>)
    }
    pub fn show_footer(
        self,
        ctx: &egui::Context,
        contents: impl FnOnce(&mut egui::Ui),
        footer: impl FnOnce(&mut egui::Ui),
    ) -> bool {
        self.show_inner(ctx, contents, Some(footer))
    }
    fn show_inner<F: FnOnce(&mut egui::Ui)>(
        self,
        ctx: &egui::Context,
        contents: impl FnOnce(&mut egui::Ui),
        footer: Option<F>,
    ) -> bool {
        if self.open.as_deref() == Some(&false) {
            return false;
        }
        let mut close = false;
        let popup_was_open = egui::Popup::is_any_open(ctx);
        let alpha = appearance(ctx, egui::Id::unique(("modal-fade", &self.title)));
        let footer_height = if footer.is_some() { 44.0 } else { 0.0 };
        let body = |ui: &mut egui::Ui| {
            ui.multiply_opacity(alpha);
            ui.set_max_height((ctx.content_rect().height() - 80.0).max(100.0));
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
            ui.add_space(12.0);
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("dialog_body")
                .auto_shrink([false, true])
                .max_height((ctx.content_rect().height() - 160.0 - footer_height).max(80.0));
            if footer_height > 0.0 {
                scroll = scroll.content_margin(egui::Margin {
                    left: 0,
                    right: SCROLL_LANE as i8,
                    top: 0,
                    bottom: 0,
                });
            }
            scroll.show(ui, contents);
            if let Some(footer) = footer {
                ui.add_space(12.0);
                footer(ui);
            }
        };
        if self.anchored {
            let p = Palette::for_dark(ctx.global_style().visuals.dark_mode);
            self.window
                .resizable(false)
                .movable(false)
                .frame(
                    egui::Frame::window(&ctx.global_style())
                        .corner_radius(RADIUS_SURFACE)
                        .shadow(surface_shadow(p)),
                )
                .show(ctx, body);
        } else {
            let dark = ctx.global_style().visuals.dark_mode;
            let modal_id = egui::Id::unique(("modal", &self.title));
            let response = egui::Modal::new(modal_id)
                .area(egui::Modal::default_area(modal_id).default_size(egui::vec2(
                    self.width + 40.0,
                    ctx.content_rect().height() - 32.0,
                )))
                .backdrop_color(Color32::from_black_alpha(
                    ((if dark { 140.0 } else { 89.0 }) * alpha) as u8,
                ))
                .frame(
                    egui::Frame::window(&ctx.global_style())
                        .fill(Palette::for_dark(dark).card.gamma_multiply(alpha))
                        .stroke(Stroke::new(
                            1.0,
                            Palette::for_dark(dark).border.gamma_multiply(alpha),
                        ))
                        .shadow(egui::epaint::Shadow {
                            offset: [0, 16],
                            blur: 48,
                            spread: 0,
                            color: Color32::from_black_alpha(
                                ((if dark { 102.0 } else { 51.0 }) * alpha) as u8,
                            ),
                        }),
                )
                .show(ctx, body);
            close |= modal_should_close(&response);
        }
        // Esc closes the dialog unless a menu or combo box is open and takes it first.
        close |= self.anchored && !popup_was_open && escape_to_close(ctx);
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
            footer(ui, |ui| {
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Ghost,
    Danger,
    DangerFilled,
}

/// A single painter-backed action control shared by toolbars, forms and menus.
pub struct Button<'a> {
    text: &'a str,
    variant: ButtonStyle,
    icon: Option<&'a str>,
    min_size: egui::Vec2,
    trailing: Option<&'a str>,
}
impl<'a> Button<'a> {
    pub fn icon(mut self, glyph: &'a str) -> Self {
        self.icon = Some(glyph);
        self
    }
    pub fn min_size(mut self, size: egui::Vec2) -> Self {
        self.min_size = size;
        self
    }
    pub fn right_text(mut self, text: &'a str) -> Self {
        self.trailing = Some(text);
        self
    }
}
impl egui::Widget for Button<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let p = Palette::of(ui);
        let text =
            ui.painter()
                .layout_no_wrap(self.text.to_owned(), medium(BUTTON), Color32::PLACEHOLDER);
        let leading = if self.icon.is_some() { 22.0 } else { 0.0 };
        let trailing = if self.trailing.is_some() { 24.0 } else { 0.0 };
        let size = egui::vec2(
            text.size().x + 24.0 + leading + trailing,
            self.min_size.y.max(COMPACT_HEIGHT),
        )
        .max(self.min_size);
        let (rect, response) = ui.allocate_at_least(size, egui::Sense::click());
        let (base, hover, ink, border) = match self.variant {
            ButtonStyle::Primary => (p.accent, p.accent_hover, p.on_accent, Stroke::NONE),
            ButtonStyle::Secondary => (
                Color32::TRANSPARENT,
                p.hover,
                p.text,
                Stroke::new(1.0, p.border_strong),
            ),
            ButtonStyle::Ghost => (Color32::TRANSPARENT, p.hover, p.muted, Stroke::NONE),
            ButtonStyle::Danger => (Color32::TRANSPARENT, p.hover, p.red, Stroke::NONE),
            ButtonStyle::DangerFilled => (
                p.red,
                p.red.gamma_multiply(0.85),
                Color32::WHITE,
                Stroke::NONE,
            ),
        };
        let hovering = ui.ctx().animate_bool_with_time(
            response.id.with("hover"),
            response.hovered() && ui.is_enabled(),
            0.12,
        );
        let pressing = ui.ctx().animate_bool_with_time(
            response.id.with("press"),
            response.is_pointer_button_down_on() && ui.is_enabled(),
            0.12,
        );
        let fill = mix(
            mix(base, hover, hovering),
            if self.variant == ButtonStyle::Primary {
                p.accent_hover
            } else {
                p.selected
            },
            pressing,
        );
        ui.painter()
            .rect(rect, RADIUS_CONTROL, fill, border, egui::StrokeKind::Inside);
        focus_ring(ui, &response, RADIUS_CONTROL);
        let ink = if !ui.is_enabled() {
            p.tertiary
        } else if self.variant == ButtonStyle::Ghost && response.hovered() {
            p.text
        } else {
            ink
        };
        let x = rect.left() + (rect.width() - text.size().x - leading - trailing) / 2.0;
        if let Some(icon) = self.icon {
            paint_icon(ui, egui::pos2(x + 8.0, rect.center().y), icon, 16.0, ink);
        }
        ui.painter().galley(
            egui::pos2(x + leading, rect.center().y - text.size().y / 2.0),
            text,
            ink,
        );
        if let Some(trailing) = self.trailing {
            paint_icon(
                ui,
                rect.right_center() - egui::vec2(12.0, 0.0),
                trailing,
                16.0,
                ink,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), self.text)
        });
        response
    }
}
pub fn button(ui: &mut egui::Ui, text: &str, variant: ButtonStyle) -> egui::Response {
    ui.add(styled_button(Palette::of(ui), text, variant))
}
pub fn styled_button<'a>(_: Palette, text: &'a str, variant: ButtonStyle) -> Button<'a> {
    let variant = if variant == ButtonStyle::Ghost && text.starts_with("Удалить") {
        ButtonStyle::Danger
    } else {
        variant
    };
    let icon = match text {
        "Новая запись"
        | "Стоп"
        | "Копировать"
        | "Копировать всё"
        | "Удалить"
        | "Переименовать"
        | "Обновить"
        | "Объединить собеседников…" => Some(icon_for_label(text)),
        _ => None,
    };
    Button {
        text,
        variant,
        icon,
        min_size: egui::vec2(0.0, CONTROL_HEIGHT),
        trailing: None,
    }
}
/// Shared menu anchor preserves egui's popup keyboard handling.
pub fn menu_button<R>(
    ui: &mut egui::Ui,
    button: Button<'_>,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> (egui::Response, Option<egui::InnerResponse<R>>) {
    let response = ui.add(button);
    let inner = egui::Popup::menu(&response).show(content);
    (response, inner)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let blend = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t) as u8;
    Color32::from_rgba_premultiplied(
        blend(a.r(), b.r()),
        blend(a.g(), b.g()),
        blend(a.b(), b.b()),
        blend(a.a(), b.a()),
    )
}
/// Focus is independent of hover. egui only focuses action widgets via keyboard/accessibility.
fn focus_ring(ui: &egui::Ui, response: &egui::Response, radius: u8) {
    if response.has_focus() && ui.is_enabled() {
        ui.painter().rect_stroke(
            response.rect.expand(1.0),
            radius,
            Stroke::new(2.0, Palette::of(ui).accent),
            egui::StrokeKind::Outside,
        );
    }
}

pub fn section_caption(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(semibold(SMALL))
            .color(Palette::of(ui).tertiary),
    );
}
pub fn page_heading(ui: &mut egui::Ui, text: &str) {
    ui.heading(text);
    ui.add_space(SPACE[4]);
}
/// Quiet list group, shared by secondary screens and dialogs.
pub fn card(p: Palette, selected: bool) -> egui::Frame {
    egui::Frame::new()
        .fill(if selected { p.selected } else { p.card })
        .corner_radius(RADIUS_SURFACE)
        .inner_margin(egui::Margin::symmetric(0, 8))
}

pub fn segmented<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    selected: &mut T,
    values: &[(T, &str)],
) -> bool {
    segmented_width(ui, selected, values, None)
}
pub fn segmented_full<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    selected: &mut T,
    values: &[(T, &str)],
) -> bool {
    segmented_width(ui, selected, values, Some(ui.available_width()))
}
fn segmented_width<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    selected: &mut T,
    values: &[(T, &str)],
    width: Option<f32>,
) -> bool {
    let mut clicked = false;
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.soft)
        .corner_radius(RADIUS_CONTROL)
        .inner_margin(4)
        .show(ui, |ui| {
            if let Some(width) = width {
                ui.set_width((width - 8.0).max(0.0));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for &(value, label) in values {
                    let text = ui.painter().layout_no_wrap(
                        label.into(),
                        medium(BUTTON),
                        Color32::PLACEHOLDER,
                    );
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(
                            width.map_or(text.size().x + 24.0, |width| {
                                (width - 8.0) / values.len() as f32
                            }),
                            24.0,
                        ),
                        egui::Sense::click(),
                    );
                    let active = *selected == value;
                    let hover = ui.ctx().animate_bool_with_time(
                        response.id.with("hover"),
                        response.hovered(),
                        0.12,
                    );
                    if active {
                        ui.painter().add(egui::epaint::RectShape::filled(
                            rect.translate(egui::vec2(0.0, 1.0)),
                            4,
                            Color32::from_black_alpha(12),
                        ));
                        ui.painter().rect_filled(rect, 4, p.card);
                    } else {
                        ui.painter().rect_filled(
                            rect,
                            4,
                            mix(Color32::TRANSPARENT, p.hover, hover),
                        );
                    }
                    ui.painter().galley(
                        rect.center() - text.size() / 2.0,
                        text,
                        if active { p.text } else { p.muted },
                    );
                    focus_ring(ui, &response, 4);
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::RadioButton,
                            ui.is_enabled(),
                            active,
                            label,
                        )
                    });
                    if response.clicked() {
                        *selected = value;
                        clicked = true;
                    }
                }
            });
        });
    clicked
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
        let height = 3.0 + 13.0 * amplitude * (time * 3.0 + n as f32 * 1.2).sin().abs();
        let x = rect.left() + 1.5 + n as f32 * 5.0;
        ui.painter().rect_filled(
            egui::Rect::from_center_size(egui::pos2(x, rect.center().y), egui::vec2(3.0, height)),
            255,
            p.accent,
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
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    if !selected {
        ui.painter().rect_filled(
            response.rect,
            RADIUS_CONTROL,
            mix(Color32::TRANSPARENT, p.hover, hover),
        );
    }
    focus_ring(ui, &response, RADIUS_CONTROL);
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
    settings_row_desc(ui, label, None, value)
}

pub fn toggle(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    setting_row(ui, label, |ui| switch(ui, value, label))
}
pub fn switch(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let p = Palette::of(ui);
    let (_, mut response) = ui.allocate_exact_size(egui::vec2(34.0, 28.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let rect = egui::Rect::from_center_size(response.rect.center(), egui::vec2(34.0, 20.0));
    let t = ui
        .ctx()
        .animate_bool_with_time(response.id.with("knob"), *value, 0.16);
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    let track = mix(p.border_strong, p.accent, t);
    ui.painter().rect_filled(
        rect,
        255,
        mix(
            track,
            if *value { p.accent_hover } else { p.selected },
            hover * 0.5,
        ),
    );
    focus_ring(ui, &response, 255);
    ui.painter().circle_filled(
        rect.center() + egui::vec2(-7.0 + 14.0 * t, 1.0),
        8.0,
        Color32::from_black_alpha(24),
    );
    ui.painter().circle_filled(
        rect.center() + egui::vec2(-7.0 + 14.0 * t, 0.0),
        8.0,
        Color32::WHITE,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *value, label)
    });
    response
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(RADIUS_MODAL)
        .shadow(surface_shadow(p))
        .inner_margin(egui::Margin::symmetric(12, 6))
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
                    FontId::new(BODY, FontFamily::Name("medium".into())),
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
        "Копировать" => icons::COPY,
        "Копировать всё" => icons::FILES,
        "Редактировать мои заметки" => icons::PENCIL,
        "Редактировать конспект" => icons::FILE_PEN,
        "Читать документ" => icons::BOOK_OPEN,
        "Копировать конспект и транскрипт" => icons::FILES,
        "Пересоздать конспект" => icons::REFRESH_CW,
        "Задачи" => icons::CHECK,
        "Экспорт" => icons::DOWNLOAD,
        "Диагностика" => icons::INFO,
        "Обновить" => icons::REFRESH_CW,
        "Объединить собеседников…" => icons::USERS,
        "Ошибка" => icons::TRIANGLE_ALERT,
        "База" => icons::DATABASE,
        "Новая запись" => icons::MIC,
        "Стоп" => icons::SQUARE,
        "Назад" => icons::CHEVRON_LEFT,
        "Добавить" => icons::PLUS,
        text if text.starts_with("Удалить") => icons::TRASH_2,
        text if text.starts_with("Переименовать") || text.starts_with("Редактировать") => {
            icons::PENCIL
        }
        text if text.starts_with("Копировать") => icons::COPY,
        text if text.starts_with("Экспорт") => icons::DOWNLOAD,
        "Показать в папке" => icons::FOLDER_OPEN,
        "Отправить задачи в трекер…" => icons::SEND,
        _ => icons::FILE_TEXT,
    }
}
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, tooltip: &str) -> egui::Response {
    icon_button_with_color(ui, glyph, tooltip, Palette::of(ui).muted, true)
}
pub fn icon_button_visible(
    ui: &mut egui::Ui,
    glyph: &str,
    tooltip: &str,
    visible: bool,
) -> egui::Response {
    icon_button_with_color(ui, glyph, tooltip, Palette::of(ui).muted, visible)
}
pub fn icon_button_with_color(
    ui: &mut egui::Ui,
    glyph: &str,
    tooltip: &str,
    color: Color32,
    visible: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(COMPACT_HEIGHT, COMPACT_HEIGHT),
        egui::Sense::click(),
    );
    let p = Palette::of(ui);
    let t = ui.ctx().animate_bool_with_time(
        response.id.with("hover"),
        response.hovered() && ui.is_enabled(),
        0.12,
    );
    let down = ui.ctx().animate_bool_with_time(
        response.id.with("press"),
        response.is_pointer_button_down_on() && ui.is_enabled(),
        0.12,
    );
    if visible || response.has_focus() || ui.input(|i| i.has_touch_screen()) {
        ui.painter().rect_filled(
            rect,
            RADIUS_CONTROL,
            mix(mix(Color32::TRANSPARENT, p.hover, t), p.selected, down),
        );
        focus_ring(ui, &response, RADIUS_CONTROL);
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
                color
            },
        );
    }
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
pub struct Disclosure {
    title: egui::WidgetText,
    default_open: bool,
    status: Option<String>,
}
pub fn disclosure(title: impl Into<egui::WidgetText>) -> Disclosure {
    Disclosure {
        title: title.into(),
        default_open: false,
        status: None,
    }
}
impl Disclosure {
    pub fn status(mut self, status: &str) -> Self {
        self.status = Some(status.into());
        self
    }
    pub fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }
    pub fn show<R>(
        self,
        ui: &mut egui::Ui,
        contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<R>> {
        ui.scope(|ui| {
            ui.style_mut().animation_time = 0.16;
            let label = self.title.text().to_owned();
            let id = ui.make_persistent_id(&label);
            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
                ui.ctx(),
                id,
                self.default_open,
            );
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(
                    ui.available_width(),
                    if self.status.is_some() { 44.0 } else { 32.0 },
                ),
                egui::Sense::click(),
            );
            let p = Palette::of(ui);
            let hover = ui.ctx().animate_bool_with_time(
                response.id.with("hover"),
                response.hovered(),
                0.12,
            );
            ui.painter().rect_filled(
                rect,
                RADIUS_CONTROL,
                mix(Color32::TRANSPARENT, p.hover, hover),
            );
            if response.clicked() {
                state.toggle(ui);
            }
            let t = ui
                .ctx()
                .animate_bool_with_time(id.with("chevron"), state.is_open(), 0.16);
            let galley = ui.painter().layout_no_wrap(
                icons::CHEVRON_RIGHT.into(),
                FontId::new(16.0, FontFamily::Name("icons".into())),
                p.muted,
            );
            let center = if self.status.is_some() {
                rect.right_center() - egui::vec2(8.0, 0.0)
            } else {
                rect.left_center() + egui::vec2(12.0, 0.0)
            };
            let status_width = self.status.as_ref().map_or(0.0, |status| {
                let text = ui.painter().layout_no_wrap(
                    status.clone(),
                    FontId::proportional(12.0),
                    p.muted,
                );
                let width = (text.size().x + 16.0).min(160.0);
                let badge = egui::Rect::from_center_size(
                    rect.right_center() - egui::vec2(28.0 + width / 2.0, 0.0),
                    egui::vec2(width, 24.0),
                );
                ui.painter().rect_filled(badge, 255, p.soft);
                ui.painter()
                    .with_clip_rect(badge.shrink2(egui::vec2(8.0, 0.0)))
                    .galley(badge.center() - text.size() / 2.0, text, p.muted);
                width + 36.0
            });
            ui.painter().add(
                egui::epaint::TextShape::new(center - galley.size() / 2.0, galley, p.muted)
                    .with_angle_and_anchor(
                        t * std::f32::consts::FRAC_PI_2,
                        egui::Align2::CENTER_CENTER,
                    ),
            );
            let text = self.title.into_galley(
                ui,
                Some(egui::TextWrapMode::Truncate),
                rect.width() - 40.0 - status_width,
                medium(BUTTON),
            );
            ui.painter().galley(
                rect.left_center()
                    + egui::vec2(
                        if self.status.is_some() { 0.0 } else { 28.0 },
                        -text.size().y / 2.0,
                    ),
                text,
                p.text,
            );
            focus_ring(ui, &response, RADIUS_CONTROL);
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::DisclosureTriangle, ui.is_enabled(), &label)
            });
            ui.ctx()
                .accesskit_node_builder(response.id, |node| node.set_expanded(state.is_open()));
            let padding = if self.status.is_some() { 16.0 } else { 0.0 };
            state.show_body_unindented(ui, |ui| {
                ui.add_space(padding);
                let result = contents(ui);
                ui.add_space(padding);
                result
            })
        })
        .inner
    }
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
    let label = label.into();
    let mut response = menu_item(ui, "", label.text(), false, selected);
    if response.clicked() && !selected {
        *current = value;
        response.mark_changed();
    }
    response
}
/// Icon, label and an optional trailing check share one keyboard target.
pub fn menu_item(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    destructive: bool,
    checked: bool,
) -> egui::Response {
    let p = Palette::of(ui);
    let text = ui.painter().layout_no_wrap(
        label.into(),
        FontId::proportional(BUTTON),
        Color32::PLACEHOLDER,
    );
    let width = ui.available_width().max(text.size().x + 64.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    let down = ui.ctx().animate_bool_with_time(
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        0.12,
    );
    ui.painter().rect_filled(
        rect,
        RADIUS_CONTROL,
        mix(mix(Color32::TRANSPARENT, p.hover, hover), p.selected, down),
    );
    let ink = if !ui.is_enabled() {
        p.tertiary
    } else if destructive {
        p.red
    } else {
        p.text
    };
    if !icon.is_empty() {
        paint_icon(
            ui,
            rect.left_center() + egui::vec2(16.0, 0.0),
            icon,
            16.0,
            ink,
        );
    }
    let inset = if icon.is_empty() { 12.0 } else { 30.0 };
    ui.painter()
        .with_clip_rect(rect.shrink2(egui::vec2(8.0, 0.0)))
        .galley(
            rect.left_center() + egui::vec2(inset, -text.size().y / 2.0),
            text,
            ink,
        );
    if checked {
        paint_icon(
            ui,
            rect.right_center() - egui::vec2(16.0, 0.0),
            icons::CHECK,
            16.0,
            ink,
        );
    }
    focus_ring(ui, &response, RADIUS_CONTROL);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), checked, label)
    });
    response
}

pub fn surface_shadow(p: Palette) -> egui::epaint::Shadow {
    egui::epaint::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if p.bg.r() < 128 { 102 } else { 31 }),
    }
}
pub fn text_field(value: &mut dyn egui::TextBuffer) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(value)
        .font(FontId::proportional(BUTTON))
        .margin(egui::Margin::symmetric(12, 7))
        .min_size(egui::vec2(0.0, CONTROL_HEIGHT))
}
pub fn select(id: impl egui::AsIdSalt) -> egui::ComboBox<'static> {
    egui::ComboBox::from_id_salt(id)
        .icon(select_icon)
        .truncate()
}
/// Returns whether the optional primary action was clicked.
pub fn empty_state(
    ui: &mut egui::Ui,
    icon: &str,
    title: &str,
    hint: &str,
    action: Option<(&str, bool)>,
) -> bool {
    let p = Palette::of(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(SPACE[5]);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
        paint_icon(ui, rect.center(), icon, 28.0, p.muted);
        ui.label(egui::RichText::new(title).font(semibold(16.0)));
        ui.label(egui::RichText::new(hint).size(14.0).color(p.muted));
        ui.add_space(SPACE[2]);
        action.is_some_and(|(label, enabled)| {
            ui.add_enabled(enabled, styled_button(p, label, ButtonStyle::Primary))
                .clicked()
        })
    })
    .inner
}

pub fn callout<R>(ui: &mut egui::Ui, icon: &str, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.soft)
        .corner_radius(RADIUS_SURFACE)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 24.0), egui::Sense::hover());
                paint_icon(ui, rect.center(), icon, 16.0, p.muted);
                ui.vertical(contents).inner
            })
            .inner
        })
        .inner
}

/// Starts at zero on appearance; egui schedules frames only until the animation settles.
pub fn appearance(ctx: &egui::Context, id: egui::Id) -> f32 {
    let frame = ctx.cumulative_frame_nr();
    let last = ctx.data_mut(|data| {
        let last = data.get_temp::<u64>(id.with("seen"));
        data.insert_temp(id.with("seen"), frame);
        last
    });
    if last.is_none_or(|last| frame.saturating_sub(last) > 1) {
        ctx.animate_value_with_time(id, 0.0, 0.0);
    }
    ctx.animate_value_with_time(id, 1.0, 0.18)
}

pub struct ToastResponse {
    pub close: bool,
    pub action: bool,
}
pub fn toast(
    ui: &mut egui::Ui,
    id: egui::Id,
    level: ToastLevel,
    text: &str,
    count: u64,
    action: Option<&str>,
) -> ToastResponse {
    let p = Palette::of(ui);
    let alpha = appearance(ui.ctx(), id.with("fade"));
    let hovered = ui
        .ctx()
        .data(|data| data.get_temp::<egui::Rect>(id))
        .is_some_and(|rect| ui.rect_contains_pointer(rect));
    let mut result = ToastResponse {
        close: false,
        action: false,
    };
    let frame = ui
        .push_id(id, |ui| {
            egui::Frame::new()
                .fill(p.card)
                .corner_radius(RADIUS_SURFACE)
                .stroke(Stroke::new(1.0, p.border))
                .shadow(surface_shadow(p))
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.multiply_opacity(alpha);
                    ui.horizontal_top(|ui| {
                        let glyph = match level {
                            ToastLevel::Info => icons::INFO,
                            ToastLevel::Success => icons::CHECK,
                            ToastLevel::Warning => icons::TRIANGLE_ALERT,
                            ToastLevel::Error => icons::CIRCLE_ALERT,
                        };
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(16.0, 28.0), egui::Sense::hover());
                        paint_icon(ui, rect.center(), glyph, 16.0, level.color(p));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            ui.scope(|ui| {
                                let focused = ui.memory(|mem| mem.has_focus(ui.next_auto_id()));
                                if !hovered && !focused && !ui.input(|i| i.has_touch_screen()) {
                                    ui.multiply_opacity(0.0);
                                }
                                result.close = icon_button(ui, icons::X, "Закрыть уведомление")
                                    .on_hover_text("Закрыть уведомление")
                                    .clicked();
                            });
                            ui.vertical(|ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.add(
                                        egui::Label::new(egui::RichText::new(text).size(14.0))
                                            .wrap(),
                                    );
                                    if count > 1 {
                                        ui.label(
                                            egui::RichText::new(format!("×{count}"))
                                                .size(12.0)
                                                .color(p.tertiary),
                                        );
                                    }
                                });
                                if let Some(action) = action {
                                    result.action =
                                        button(ui, action, ButtonStyle::Ghost).clicked();
                                }
                            });
                        });
                    });
                })
                .response
        })
        .inner;
    ui.ctx().data_mut(|data| data.insert_temp(id, frame.rect));
    result
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    fn escape_input() -> egui::RawInput {
        egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        }
    }
    #[test]
    fn escape_first_leaves_text_edit_then_closes_page() {
        for check_before_field in [true, false] {
            let ctx = egui::Context::default();
            install(&ctx);
            let mut draft = "Draft".to_owned();
            ctx.run_ui(Default::default(), |ui| {
                ui.text_edit_singleline(&mut draft).request_focus();
            })
            .drop_without_applying_deltas();
            assert!(ctx.text_edit_focused());
            ctx.run_ui(escape_input(), |ui| {
                if check_before_field {
                    assert!(!escape_to_close(ui.ctx()));
                }
                ui.text_edit_singleline(&mut draft);
                if !check_before_field {
                    assert!(!escape_to_close(ui.ctx()));
                }
                if ui.ctx().current_pass_index() == 0 {
                    ui.ctx()
                        .request_discard("Check the focus guard across layout passes");
                }
            })
            .drop_without_applying_deltas();
            assert!(!ctx.text_edit_focused());
            ctx.run_ui(escape_input(), |ui| {
                assert!(escape_to_close(ui.ctx()));
                ui.text_edit_singleline(&mut draft);
            })
            .drop_without_applying_deltas();
            assert_eq!(draft, "Draft");
        }
    }
    #[test]
    fn escape_first_unfocuses_rename_field_without_closing_modal() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut draft = "New name".to_owned();
        let mut open = true;
        for _ in 0..2 {
            ctx.run_pass(Default::default(), |ctx| {
                modal(ctx, "Rename").open(&mut open).show(ctx, |ui| {
                    ui.text_edit_singleline(&mut draft).request_focus();
                });
            })
            .drop_without_applying_deltas();
        }
        assert!(ctx.text_edit_focused());
        ctx.run_pass(escape_input(), |ctx| {
            assert!(!modal(ctx, "Rename").open(&mut open).show(ctx, |ui| {
                ui.text_edit_singleline(&mut draft);
            }));
        })
        .drop_without_applying_deltas();
        assert!(open);
        assert!(!ctx.text_edit_focused());
        assert_eq!(draft, "New name");
        ctx.run_pass(escape_input(), |ctx| {
            assert!(modal(ctx, "Rename").open(&mut open).show(ctx, |ui| {
                ui.text_edit_singleline(&mut draft);
            }));
        })
        .drop_without_applying_deltas();
        assert!(!open);
    }
    #[test]
    fn escape_guard_also_protects_direct_modals() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut draft = "Name".to_owned();
        let mut draw = |ctx: &egui::Context, focus: bool| {
            let response = egui::Modal::new(egui::Id::unique("onboarding-test")).show(ctx, |ui| {
                let response = ui.text_edit_singleline(&mut draft);
                if focus {
                    response.request_focus();
                }
            });
            modal_should_close(&response)
        };
        for _ in 0..2 {
            ctx.run_pass(Default::default(), |ctx| {
                draw(ctx, true);
            })
            .drop_without_applying_deltas();
        }
        ctx.run_pass(escape_input(), |ctx| {
            assert!(!draw(ctx, false));
        })
        .drop_without_applying_deltas();
        ctx.run_pass(escape_input(), |ctx| {
            assert!(draw(ctx, false));
        })
        .drop_without_applying_deltas();
    }
    fn enter() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }
    #[test]
    fn keyboard_activates_actions_and_disabled_actions_stay_inert() {
        for enabled in [true, false] {
            let ctx = egui::Context::default();
            install(&ctx);
            ctx.run_ui(Default::default(), |ui| {
                ui.add_enabled(
                    enabled,
                    styled_button(Palette::of(ui), "Копировать", ButtonStyle::Secondary),
                )
                .request_focus();
            })
            .drop_without_applying_deltas();
            let mut clicked = false;
            ctx.run_ui(
                egui::RawInput {
                    events: vec![enter()],
                    ..Default::default()
                },
                |ui| {
                    clicked = ui
                        .add_enabled(
                            enabled,
                            styled_button(Palette::of(ui), "Копировать", ButtonStyle::Secondary),
                        )
                        .clicked();
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(clicked, enabled);
        }
    }
    #[test]
    fn escape_closes_modal_but_not_through_an_open_popup() {
        for popup in [false, true] {
            let ctx = egui::Context::default();
            install(&ctx);
            for _ in 0..2 {
                ctx.run_pass(Default::default(), |ctx| {
                    modal(ctx, "Подтверждение").show(ctx, |ui| {
                        ui.label("Проверка");
                    });
                })
                .drop_without_applying_deltas();
            }
            let mut closed = false;
            ctx.run_pass(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ctx| {
                    if popup {
                        egui::Popup::open_id(ctx, egui::Id::unique("test-popup"));
                    }
                    closed = modal(ctx, "Подтверждение").show(ctx, |ui| {
                        ui.label("Проверка");
                    });
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(closed, !popup);
        }
    }
    #[test]
    fn toast_close_is_a_pointer_action() {
        let ctx = egui::Context::default();
        install(&ctx);
        let id = egui::Id::unique("test-toast");
        let render = |events| {
            let mut closed = false;
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 300.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    ui.set_max_width(340.0);
                    closed = toast(ui, id, ToastLevel::Success, "Скопировано", 1, None).close;
                },
            )
            .drop_without_applying_deltas();
            closed
        };
        render(vec![]);
        render(vec![]);
        let rect = ctx.data(|data| data.get_temp::<egui::Rect>(id)).unwrap();
        let pos = egui::pos2(rect.right() - 26.0, rect.top() + 26.0);
        render(vec![egui::Event::PointerMoved(pos)]);
        assert!(!render(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE
        }]));
        assert!(render(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE
        }]));
    }
}

pub fn row_background(ui: &egui::Ui, response: &egui::Response, selected: bool) {
    let p = Palette::of(ui);
    let t =
        ui.ctx()
            .animate_bool_with_time(response.id.with("row-hover"), response.hovered(), 0.12);
    ui.painter().rect_filled(
        response.rect,
        RADIUS_CONTROL,
        if selected {
            p.selected
        } else {
            mix(Color32::TRANSPARENT, p.hover, t)
        },
    );
}

/// A bounded footer row keeps right-aligned actions from consuming the modal's height.
pub fn footer<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), contents)
            .inner
    })
    .inner
}

#[cfg(test)]
mod footer_tests {
    use super::*;
    #[test]
    fn modal_footer_uses_one_row_in_a_tall_viewport() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 800.0),
                )),
                ..Default::default()
            },
            |ui| {
                let before = ui.cursor().top();
                footer(ui, |ui| {
                    button(ui, "Удалить", ButtonStyle::DangerFilled);
                    button(ui, "Отмена", ButtonStyle::Secondary);
                });
                assert!(
                    ui.cursor().top() - before <= 48.0,
                    "Footer expanded into the modal body"
                );
            },
        )
        .drop_without_applying_deltas();
    }
}

/// Checkbox keeps a visible outline in either palette and shares keyboard activation.
pub struct Checkbox<'a> {
    value: &'a mut bool,
    label: &'a str,
}
impl<'a> Checkbox<'a> {
    pub fn new(value: &'a mut bool, label: &'a str) -> Self {
        Self { value, label }
    }
    pub fn without_text(value: &'a mut bool) -> Self {
        Self::new(value, "")
    }
}
impl egui::Widget for Checkbox<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        let p = Palette::of(ui);
        let text = ui.painter().layout_no_wrap(
            self.label.into(),
            FontId::proportional(14.0),
            Color32::PLACEHOLDER,
        );
        let width = 16.0
            + if self.label.is_empty() {
                0.0
            } else {
                8.0 + text.size().x
            };
        let (rect, mut response) =
            ui.allocate_exact_size(egui::vec2(width, 24.0), egui::Sense::click());
        if response.clicked() {
            *self.value = !*self.value;
            response.mark_changed();
        }
        let square = egui::Rect::from_center_size(
            rect.left_center() + egui::vec2(8.0, 0.0),
            egui::vec2(16.0, 16.0),
        );
        ui.painter().rect(
            square,
            4,
            if *self.value {
                p.accent
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(
                1.0,
                if response.hovered() {
                    p.tertiary
                } else {
                    p.border_strong
                },
            ),
            egui::StrokeKind::Inside,
        );
        if *self.value {
            paint_icon(ui, square.center(), icons::CHECK, 12.0, Color32::WHITE);
        }
        focus_ring(ui, &response.clone().with_new_rect(square), 4);
        if !self.label.is_empty() {
            ui.painter().galley(
                rect.left_center() + egui::vec2(24.0, -text.size().y / 2.0),
                text,
                p.text,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::Role::CheckBox,
                ui.is_enabled(),
                *self.value,
                self.label,
            )
        });
        response
    }
}
pub fn toggle_chip(ui: &mut egui::Ui, value: &mut bool, label: &str) -> egui::Response {
    let p = Palette::of(ui);
    let text = ui
        .painter()
        .layout_no_wrap(label.into(), medium(14.0), Color32::PLACEHOLDER);
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(text.size().x + 24.0, 32.0), egui::Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    ui.painter().rect_filled(
        rect,
        6,
        if *value {
            p.selected
        } else {
            mix(p.soft, p.hover, hover)
        },
    );
    ui.painter().galley(
        rect.center() - text.size() / 2.0,
        text,
        if *value { p.text } else { p.muted },
    );
    focus_ring(ui, &response, 6);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *value, label)
    });
    response
}
pub fn form_field<R>(
    ui: &mut egui::Ui,
    label: &str,
    description: Option<&str>,
    control: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let gap = ui.spacing().item_spacing.y;
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let p = Palette::of(ui);
        ui.label(egui::RichText::new(label).font(medium(13.0)).color(p.muted));
        if let Some(description) = description {
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(egui::RichText::new(description).size(12.0).color(p.muted)).wrap(),
            );
        }
        ui.add_space(6.0);
        ui.spacing_mut().text_edit_width = ui.available_width();
        let result = control(ui);
        ui.add_space((16.0 - gap).max(0.0));
        result
    })
    .inner
}
pub fn search_input(
    ui: &mut egui::Ui,
    text: &mut String,
    id: impl egui::AsIdSalt,
    placeholder: &str,
    shortcut: Option<&str>,
) -> egui::Response {
    let p = Palette::of(ui);
    egui::Frame::new()
        .fill(p.bg)
        .stroke(Stroke::new(1.0, p.border_strong))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 32.0), egui::Sense::hover());
                paint_icon(ui, rect.center(), icons::SEARCH, 16.0, p.muted);
                let reserved = shortcut.map_or(0.0, |s| {
                    ui.painter()
                        .layout_no_wrap(s.into(), FontId::proportional(12.0), p.tertiary)
                        .size()
                        .x
                        + 8.0
                });
                let response = ui.add(
                    egui::TextEdit::singleline(text)
                        .id_salt(id)
                        .font(FontId::proportional(14.0))
                        .frame(egui::Frame::NONE)
                        .margin(0)
                        .align(egui::Align2::LEFT_CENTER)
                        .min_size(egui::vec2(0.0, 32.0))
                        .desired_width((ui.available_width() - reserved).max(24.0))
                        .hint_text(placeholder),
                );
                if let Some(shortcut) = shortcut {
                    ui.label(egui::RichText::new(shortcut).size(12.0).color(p.tertiary));
                }
                response
            })
            .inner
        })
        .inner
}
/// Shared fixed columns for past and upcoming meetings.
pub fn meeting_row(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    time: &str,
    title: &str,
    meta: &str,
    action: bool,
    selected: bool,
) -> (egui::Response, bool) {
    let p = Palette::of(ui);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 40.0),
        egui::Sense::click_and_drag(),
    );
    row_background(ui, &response, selected);
    ui.painter().text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        time,
        FontId::proportional(13.0),
        p.muted,
    );
    let mut record = false;
    let action_width = if action { 100.0 } else { 0.0 };
    let meta_width = if meta.is_empty() {
        0.0
    } else {
        ui.painter()
            .layout_no_wrap(meta.into(), FontId::proportional(12.0), p.tertiary)
            .size()
            .x
            .min(140.0)
            + 16.0
    };
    let mut job = egui::text::LayoutJob::simple(
        title.into(),
        medium(14.0),
        p.text,
        (rect.width() - 70.0 - meta_width - action_width - 8.0).max(1.0),
    );
    job.wrap.max_rows = 1;
    let text = ui.painter().layout_job(job);
    ui.painter().galley(
        rect.left_center() + egui::vec2(70.0, -text.size().y / 2.0),
        text,
        p.text,
    );
    if !meta.is_empty() {
        ui.painter().text(
            rect.right_center() - egui::vec2(8.0 + action_width, 0.0),
            egui::Align2::RIGHT_CENTER,
            meta,
            FontId::proportional(12.0),
            p.tertiary,
        );
    }
    if action {
        ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt(id)
                .max_rect(egui::Rect::from_min_size(
                    rect.right_center() - egui::vec2(96.0, 14.0),
                    egui::vec2(96.0, 28.0),
                ))
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |ui| {
                record = ui
                    .add(
                        styled_button(p, "Записать", ButtonStyle::Ghost)
                            .min_size(egui::vec2(0.0, 28.0)),
                    )
                    .clicked();
            },
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), title));
    (response, record)
}
pub fn submenu<R>(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) {
    let p = Palette::of(ui);
    let mut job = egui::text::LayoutJob::default();
    if !icon.is_empty() {
        job.append(
            icon,
            0.0,
            egui::TextFormat {
                font_id: FontId::new(16.0, FontFamily::Name("icons".into())),
                color: p.muted,
                ..Default::default()
            },
        );
    }
    job.append(
        label,
        if icon.is_empty() { 0.0 } else { 6.0 },
        egui::TextFormat {
            font_id: FontId::proportional(14.0),
            color: p.text,
            ..Default::default()
        },
    );
    let button = egui::Button::new(job)
        .frame(false)
        .min_size(egui::vec2(ui.available_width(), 30.0))
        .right_text(
            egui::RichText::new(icons::CHEVRON_RIGHT)
                .font(FontId::new(16.0, FontFamily::Name("icons".into())))
                .color(p.tertiary),
        );
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 30.0;
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::containers::menu::SubMenuButton::from_button(button).ui(ui, body);
    });
}

pub fn settings_group<R>(
    ui: &mut egui::Ui,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    if !title.is_empty() {
        section_caption(ui, title);
        ui.add_space(8.0);
    }
    let key = egui::Id::unique("active-settings-group");
    let id = ui.scope_id().with(("group", title));
    let previous = ui.ctx().data_mut(|d| {
        let previous = d.get_temp::<egui::Id>(key);
        d.insert_temp(key, id);
        d.insert_temp(id.with("rows"), 0_usize);
        previous
    });
    let result = egui::Frame::new()
        .fill(Palette::of(ui).card)
        .corner_radius(8)
        .stroke(Stroke::new(1.0, Palette::of(ui).border))
        .inner_margin(egui::Margin::symmetric(16, 0))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            body(ui)
        })
        .inner;
    ui.ctx().data_mut(|d| {
        if let Some(previous) = previous {
            d.insert_temp(key, previous);
        } else {
            d.remove::<egui::Id>(key);
        }
    });
    ui.add_space(24.0);
    result
}
pub fn settings_disclosure<R>(
    ui: &mut egui::Ui,
    title: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) {
    settings_group(ui, "", |ui| {
        disclosure(title).show(ui, body);
    });
}
pub fn settings_row_desc<R>(
    ui: &mut egui::Ui,
    label: &str,
    description: Option<&str>,
    control: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let p = Palette::of(ui);
    let group = ui
        .ctx()
        .data(|d| d.get_temp::<egui::Id>(egui::Id::unique("active-settings-group")));
    if let Some(id) = group {
        let index = ui.ctx().data_mut(|d| {
            let index = d.get_temp::<usize>(id.with("rows")).unwrap_or(0);
            d.insert_temp(id.with("rows"), index + 1);
            index
        });
        if index > 0 {
            let mut painter = ui.painter().clone();
            painter.set_opacity(1.0);
            painter.hline(
                ui.max_rect().x_range(),
                ui.cursor().top(),
                Stroke::new(1.0, p.border),
            );
        }
    }
    let width = ui.available_width();
    let label_width = (width * 0.52).max(100.0);
    let desc = description.map(|text| {
        ui.painter().layout(
            text.into(),
            FontId::proportional(12.0),
            p.muted,
            label_width,
        )
    });
    let label_galley = ui.painter().layout(
        label.into(),
        FontId::proportional(14.0),
        p.text,
        label_width,
    );
    let height =
        (label_galley.size().y + desc.as_ref().map_or(0.0, |d| d.size().y + 4.0) + 16.0).max(44.0);
    ui.horizontal(|ui| {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(label_width, height), egui::Sense::hover());
        let text_height = label_galley.size().y + desc.as_ref().map_or(0.0, |d| d.size().y + 4.0);
        let origin = egui::pos2(rect.left(), rect.center().y - text_height / 2.0);
        let label_height = label_galley.size().y;
        ui.painter().galley(origin, label_galley, p.text);
        if let Some(desc) = desc {
            ui.painter()
                .galley(origin + egui::vec2(0.0, label_height + 4.0), desc, p.muted);
        }
        ui.allocate_ui_with_layout(
            egui::vec2((width - label_width - 8.0).max(32.0), height),
            egui::Layout::right_to_left(egui::Align::Center),
            control,
        )
        .inner
    })
    .inner
}

#[cfg(test)]
mod checkbox_tests {
    use super::*;
    #[test]
    fn keyboard_checkbox_reports_changes_and_respects_disabled_state() {
        for enabled in [true, false] {
            let ctx = egui::Context::default();
            install(&ctx);
            let mut value = false;
            ctx.run_ui(Default::default(), |ui| {
                ui.add_enabled(enabled, Checkbox::new(&mut value, "Общая"))
                    .request_focus();
            })
            .drop_without_applying_deltas();
            let mut changed = false;
            ctx.run_ui(
                egui::RawInput {
                    events: vec![egui::Event::Key {
                        key: egui::Key::Space,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    changed = ui
                        .add_enabled(enabled, Checkbox::new(&mut value, "Общая"))
                        .changed();
                },
            )
            .drop_without_applying_deltas();
            assert_eq!(value, enabled);
            assert_eq!(changed, enabled);
        }
    }
}

#[cfg(test)]
mod sticky_footer_tests {
    use super::*;
    #[test]
    fn long_form_keeps_footer_inside_a_small_viewport() {
        let ctx = egui::Context::default();
        install(&ctx);
        for _ in 0..3 {
            let mut rect = egui::Rect::NOTHING;
            ctx.run_pass(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(640.0, 427.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    modal(ctx, "Запись").show_footer(
                        ctx,
                        |ui| {
                            for _ in 0..30 {
                                ui.label("Строка формы");
                            }
                        },
                        |ui| {
                            footer(ui, |ui| {
                                rect = button(ui, "Начать запись", ButtonStyle::Primary).rect;
                            });
                        },
                    );
                },
            )
            .drop_without_applying_deltas();
            assert!(
                rect.top() >= 0.0 && rect.bottom() <= 427.0,
                "Footer clipped: {rect:?}"
            );
        }
    }
}
