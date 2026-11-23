use gpui::{
    div, prelude::*, px, size, App, Application, Bounds, ClickEvent, Context, Render, Window,
    WindowBounds, WindowOptions,
};

use crate::{
    components::{primary_button, quiet_button},
    layout,
    navigation::Page,
    state::DesktopState,
    theme::{FONT_SIZE_BODY, FONT_SIZE_TITLE, SPACING},
};

struct DesktopView {
    state: DesktopState,
}

impl DesktopView {
    fn toggle_sidebar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_sidebar();
        cx.notify();
    }

    fn open_settings(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.navigate(Page::Settings);
        cx.notify();
    }

    fn show_welcome(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.navigate(Page::Welcome);
        cx.notify();
    }

    fn show_session_information(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.navigate(Page::SessionInformation);
        cx.notify();
    }

    fn select_dark_theme(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.set_theme(crate::theme::ThemeMode::Dark);
        cx.notify();
    }

    fn select_light_theme(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.set_theme(crate::theme::ThemeMode::Light);
        cx.notify();
    }
}

impl Render for DesktopView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.state.theme.palette();
        let state = &self.state;

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.canvas)
            .text_color(palette.text)
            .text_size(px(FONT_SIZE_BODY))
            .child(
                div()
                    .h(px(56.0))
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(palette.border)
                    .bg(palette.panel)
                    .px_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(quiet_button(
                                "sidebar-toggle",
                                "☰",
                                palette,
                                cx.listener(Self::toggle_sidebar),
                            ))
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Gentle Fleet"),
                            ),
                    )
                    .child(quiet_button(
                        "settings-open",
                        "Configuración",
                        palette,
                        cx.listener(Self::open_settings),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .when(state.sidebar_open, |this| {
                        this.child(layout::sidebar(state, palette))
                    })
                    .child(render_page(state, palette, cx)),
            )
    }
}

fn render_page(
    state: &DesktopState,
    palette: crate::theme::Palette,
    cx: &mut Context<DesktopView>,
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
            .child(
                primary_button(
                    "welcome-open-session",
                    "Abrir sesión",
                    palette,
                    cx.listener(DesktopView::show_session_information),
                ),
            ),
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
                    .child(
                        quiet_button(
                            "theme-dark",
                            "Oscuro",
                            palette,
                            cx.listener(DesktopView::select_dark_theme),
                        ),
                    )
                    .child(
                        quiet_button(
                            "theme-light",
                            "Claro",
                            palette,
                            cx.listener(DesktopView::select_light_theme),
                        ),
                    ),
            )
            .child(
                quiet_button(
                    "settings-home",
                    "Volver al inicio",
                    palette,
                    cx.listener(DesktopView::show_welcome),
                ),
            ),
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
                    .child("La creación de sesiones se incorporará en P6.2-C. No se inició ningún agente."),
            )
            .child(
                quiet_button(
                    "session-back",
                    "Volver",
                    palette,
                    cx.listener(DesktopView::show_welcome),
                ),
            ),
    }
}

pub fn run() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1180.0), px(780.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(760.0), px(520.0))),
                app_id: Some("dev.gentle-fleet.desktop".to_string()),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| DesktopView {
                    state: DesktopState::default(),
                })
            },
        )
        .expect("could not open Gentle Fleet Desktop window");
        cx.activate(true);
    });
}
