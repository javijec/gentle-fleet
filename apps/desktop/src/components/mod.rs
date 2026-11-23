use gpui::{
    div, prelude::*, px, App, ClickEvent, SharedString, StatefulInteractiveElement, Window,
};

use crate::theme::{Palette, BORDER_WIDTH, RADIUS, SPACING};

pub fn quiet_button(
    label: impl Into<SharedString>,
    palette: Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    div()
        .id(label.clone())
        .px_3()
        .py(px(SPACING[1]))
        .rounded(px(RADIUS))
        .text_color(palette.muted)
        .border(px(BORDER_WIDTH))
        .border_color(palette.border)
        .focusable()
        .focus(|style| style.border_color(palette.focus))
        .hover(|style| style.bg(palette.elevated).text_color(palette.text))
        .child(label)
        .on_click(on_click)
}

pub fn primary_button(
    label: impl Into<SharedString>,
    palette: Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    div()
        .id(label.clone())
        .px_4()
        .py(px(SPACING[2]))
        .rounded(px(RADIUS))
        .bg(palette.accent)
        .border(px(BORDER_WIDTH))
        .border_color(palette.accent)
        .text_color(palette.accent_text)
        .focusable()
        .focus(|style| style.border_color(palette.focus))
        .hover(|style| style.opacity(0.88))
        .child(label)
        .on_click(on_click)
}
