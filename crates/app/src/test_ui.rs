//! Headless input helpers. Tests exercise real widgets and state channels.
use eframe::egui::{self, *};

pub fn state() -> (
    crate::state::AppState,
    std::sync::mpsc::Receiver<crate::worker::Command>,
    std::sync::mpsc::Sender<crate::worker::Event>,
) {
    let (tx, commands) = std::sync::mpsc::channel();
    let (events, rx) = std::sync::mpsc::channel();
    (
        crate::state::AppState::test_channels(tx, rx),
        commands,
        events,
    )
}
pub fn frame(ctx: &Context, events: Vec<Event>, draw: &mut impl FnMut(&mut Ui)) -> FullOutput {
    ctx.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 900.0))),
            events,
            focused: true,
            ..Default::default()
        },
        draw,
    )
}
pub fn text_rect(shapes: &[egui::epaint::ClippedShape], text: &str) -> Rect {
    fn find(shape: &egui::epaint::Shape, text: &str) -> Option<Rect> {
        match shape {
            egui::epaint::Shape::Text(shape) if shape.galley.text() == text => {
                Some(Rect::from_min_size(shape.pos, shape.galley.size()))
            }
            egui::epaint::Shape::Vec(shapes) => {
                shapes.iter().rev().find_map(|shape| find(shape, text))
            }
            _ => None,
        }
    }
    shapes
        .iter()
        .rev()
        .find_map(|shape| find(&shape.shape, text))
        .unwrap_or_else(|| panic!("Widget text not found: {text}"))
}
pub fn click(ctx: &Context, draw: &mut impl FnMut(&mut Ui), text: &str) {
    for _ in 0..2 {
        frame(ctx, vec![], draw).drop_without_applying_deltas();
    }
    let output = frame(ctx, vec![], draw);
    let pos = text_rect(&output.shapes, text).center();
    output.drop_without_applying_deltas();
    for pressed in [true, false] {
        frame(
            ctx,
            vec![
                Event::PointerMoved(pos),
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ],
            draw,
        )
        .drop_without_applying_deltas();
    }
}
pub fn replace_text(ctx: &Context, draw: &mut impl FnMut(&mut Ui), old: &str, text: &str) {
    click(ctx, draw, old);
    frame(
        ctx,
        vec![
            Event::Key {
                key: Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Modifiers {
                    ctrl: true,
                    command: true,
                    ..Modifiers::NONE
                },
            },
            Event::Text(text.into()),
        ],
        draw,
    )
    .drop_without_applying_deltas();
}
