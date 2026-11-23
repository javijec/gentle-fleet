use crate::theme::{Palette, BORDER_WIDTH, RADIUS, SPACING};
use gpui::{
    div, prelude::*, px, App, ClickEvent, FocusHandle, SharedString, StatefulInteractiveElement,
    Window,
};

pub fn quiet_button(
    id: &'static str,
    label: impl Into<SharedString>,
    palette: Palette,
    focus_handle: Option<FocusHandle>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let button = div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .cursor_pointer()
        .px_3()
        .py(px(SPACING[1]))
        .rounded(px(RADIUS))
        .text_color(palette.muted)
        .border(px(BORDER_WIDTH))
        .border_color(palette.border);
    let button = if let Some(focus_handle) = focus_handle {
        button.track_focus(&focus_handle)
    } else {
        button.focusable()
    };
    button
        .tab_stop(true)
        .focus(|style| style.border_color(palette.focus))
        .hover(|style| style.bg(palette.elevated).text_color(palette.text))
        .child(label)
        .on_click(on_click)
}

pub fn primary_button(
    id: &'static str,
    label: impl Into<SharedString>,
    palette: Palette,
    focus_handle: Option<FocusHandle>,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let label = label.into();
    let button = div()
        .id(id)
        .debug_selector(|| id.to_owned())
        .cursor_pointer()
        .px_4()
        .py(px(SPACING[2]))
        .rounded(px(RADIUS))
        .bg(palette.accent)
        .border(px(BORDER_WIDTH))
        .border_color(palette.accent)
        .text_color(palette.accent_text);
    let button = if let Some(focus_handle) = focus_handle {
        button.track_focus(&focus_handle)
    } else {
        button.focusable()
    };
    button
        .tab_stop(true)
        .focus(|style| style.border_color(palette.focus))
        .hover(|style| style.opacity(0.88))
        .child(label)
        .on_click(on_click)
}
