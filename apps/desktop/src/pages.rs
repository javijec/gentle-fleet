use gpui::{div, prelude::*, px, App, Context, FocusHandle};

use crate::{
    app::DesktopView,
    components::{primary_button, quiet_button},
    navigation::Page,
    state::DesktopState,
    theme::{Palette, FONT_SIZE_TITLE, SPACING},
};

#[derive(Clone)]
pub(crate) struct PageFocusHandles {
    pub(crate) welcome_open_session: FocusHandle,
    pub(crate) settings_dark: FocusHandle,
    pub(crate) settings_light: FocusHandle,
    pub(crate) settings_home: FocusHandle,
    pub(crate) session_back: FocusHandle,
}

impl PageFocusHandles {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            welcome_open_session: cx.focus_handle().tab_stop(true),
            settings_dark: cx.focus_handle().tab_stop(true),
            settings_light: cx.focus_handle().tab_stop(true),
            settings_home: cx.focus_handle().tab_stop(true),
            session_back: cx.focus_handle().tab_stop(true),
        }
    }
}

pub(crate) fn render_page(
    state: &DesktopState,
    palette: Palette,
    cx: &mut Context<DesktopView>,
    page_focus: PageFocusHandles,
) -> impl IntoElement {
    match state.page {
        Page::Welcome => div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SPACING[2]))
            .p_8()
            .child(
                div()
                    .text_size(px(FONT_SIZE_TITLE))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Gentle Fleet"),
            )
            .child(
                div()
                    .max_w(px(480.0))
                    .text_center()
                    .text_color(palette.muted)
                    .child("Un espacio tranquilo para continuar el trabajo de tus agentes."),
            )
            .child(primary_button(
                "welcome-open-session",
                "Abrir sesión",
                palette,
                Some(page_focus.welcome_open_session),
                cx.listener(DesktopView::show_session_information),
            )),
        Page::Settings => div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(SPACING[2]))
            .p_8()
            .child(
                div()
                    .text_size(px(FONT_SIZE_TITLE))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Configuración"),
            )
            .child(div().text_color(palette.muted).child("Apariencia"))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(quiet_button(
                        "theme-dark",
                        "Oscuro",
                        palette,
                        Some(page_focus.settings_dark),
                        cx.listener(DesktopView::select_dark_theme),
                    ))
                    .child(quiet_button(
                        "theme-light",
                        "Claro",
                        palette,
                        Some(page_focus.settings_light),
                        cx.listener(DesktopView::select_light_theme),
                    )),
            )
            .child(quiet_button(
                "settings-home",
                "Volver al inicio",
                palette,
                Some(page_focus.settings_home),
                cx.listener(DesktopView::show_welcome),
            )),
        Page::SessionInformation => div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(SPACING[2]))
            .p_8()
            .child(
                div()
                    .text_size(px(FONT_SIZE_TITLE))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Sesiones próximamente"),
            )
            .child(
                div()
                    .max_w(px(480.0))
                    .text_center()
                    .text_color(palette.muted)
                    .child(
                        "La creación de sesiones se incorporará en P6.2-C. No se inició ningún agente.",
                    ),
            )
            .child(quiet_button(
                "session-back",
                "Volver",
                palette,
                Some(page_focus.session_back),
                cx.listener(DesktopView::show_welcome),
            )),
    }
}
