use gpui::{
    div, prelude::*, px, size, App, Application, Bounds, ClickEvent, Context, FocusHandle,
    KeyDownEvent, Render, Window, WindowBounds, WindowOptions,
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
    focus_handle: FocusHandle,
    sidebar_toggle_focus: FocusHandle,
    settings_open_focus: FocusHandle,
    page_focus: PageFocusHandles,
}

#[derive(Clone)]
struct PageFocusHandles {
    welcome_open_session: FocusHandle,
    settings_dark: FocusHandle,
    settings_light: FocusHandle,
    settings_home: FocusHandle,
    session_back: FocusHandle,
}

impl DesktopView {
    fn toggle_sidebar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_sidebar();
        cx.notify();
    }

    fn open_settings(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.state.navigate(Page::Settings);
        window.focus(&self.page_focus.settings_dark);
        cx.notify();
    }

    fn show_welcome(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.state.navigate(Page::Welcome);
        window.focus(&self.page_focus.welcome_open_session);
        cx.notify();
    }

    fn show_session_information(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.navigate(Page::SessionInformation);
        window.focus(&self.page_focus.session_back);
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
        let page_focus = self.page_focus.clone();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(palette.canvas)
            .text_color(palette.text)
            .text_size(px(FONT_SIZE_BODY))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "tab" && !event.is_held {
                    window.prevent_default();
                    if event.keystroke.modifiers.shift {
                        window.focus_prev();
                    } else {
                        window.focus_next();
                    }
                    cx.stop_propagation();
                }
            }))
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
                                Some(self.sidebar_toggle_focus.clone()),
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
                        Some(self.settings_open_focus.clone()),
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
                    .child(render_page(state, palette, cx, page_focus)),
            )
    }
}

fn render_page(
    state: &DesktopState,
    palette: crate::theme::Palette,
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
            .child(
                primary_button(
                    "welcome-open-session",
                    "Abrir sesión",
                    palette,
                    Some(page_focus.welcome_open_session),
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
                            Some(page_focus.settings_dark),
                            cx.listener(DesktopView::select_dark_theme),
                        ),
                    )
                    .child(
                        quiet_button(
                            "theme-light",
                            "Claro",
                            palette,
                            Some(page_focus.settings_light),
                            cx.listener(DesktopView::select_light_theme),
                        ),
                    ),
            )
            .child(
                quiet_button(
                    "settings-home",
                    "Volver al inicio",
                    palette,
                    Some(page_focus.settings_home),
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
                    Some(page_focus.session_back),
                    cx.listener(DesktopView::show_welcome),
                ),
            ),
    }
}

pub fn run() {
    Application::new().run(|cx: &mut App| {
        // Keep the initial window within a typical 1280x720 logical Wayland
        // work area (1920x1080 at 1.5x), including Omarchy's reserved top bar.
        let bounds = Bounds::centered(None, size(px(1180.0), px(640.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(760.0), px(520.0))),
                    app_id: Some("dev.gentle-fleet.desktop".to_string()),
                    ..Default::default()
                },
                |window, cx| {
                    let focus_handle = cx.focus_handle();
                    let sidebar_toggle_focus = cx.focus_handle().tab_stop(true);
                    let settings_open_focus = cx.focus_handle().tab_stop(true);
                    let page_focus = PageFocusHandles {
                        welcome_open_session: cx.focus_handle().tab_stop(true),
                        settings_dark: cx.focus_handle().tab_stop(true),
                        settings_light: cx.focus_handle().tab_stop(true),
                        settings_home: cx.focus_handle().tab_stop(true),
                        session_back: cx.focus_handle().tab_stop(true),
                    };
                    window.focus(&focus_handle);
                    cx.new(|_| DesktopView {
                        state: DesktopState::default(),
                        focus_handle,
                        sidebar_toggle_focus,
                        settings_open_focus,
                        page_focus,
                    })
                },
            )
            .expect("could not open Gentle Fleet Desktop window");
        window
            .update(cx, |view, window, _| window.focus(&view.focus_handle))
            .expect("could not focus Gentle Fleet Desktop window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{KeyUpEvent, Keystroke, TestAppContext};

    #[gpui::test]
    fn tab_moves_focus_between_the_primary_toolbar_controls(cx: &mut TestAppContext) {
        let (view, visual) = cx.add_window_view(|window, cx| {
            let focus_handle = cx.focus_handle();
            let sidebar_toggle_focus = cx.focus_handle().tab_stop(true);
            let settings_open_focus = cx.focus_handle().tab_stop(true);
            let page_focus = PageFocusHandles {
                welcome_open_session: cx.focus_handle().tab_stop(true),
                settings_dark: cx.focus_handle().tab_stop(true),
                settings_light: cx.focus_handle().tab_stop(true),
                settings_home: cx.focus_handle().tab_stop(true),
                session_back: cx.focus_handle().tab_stop(true),
            };
            window.focus(&focus_handle);
            DesktopView {
                state: DesktopState::default(),
                focus_handle,
                sidebar_toggle_focus,
                settings_open_focus,
                page_focus,
            }
        });

        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        let sidebar_focus = visual.update(|_, cx| view.read(cx).sidebar_toggle_focus.clone());
        visual.update(|window, _| assert!(sidebar_focus.is_focused(window)));
        visual.simulate_keystrokes("tab");
        visual.run_until_parked();
        let settings_focus = visual.update(|_, cx| view.read(cx).settings_open_focus.clone());
        visual.update(|window, _| assert!(settings_focus.is_focused(window)));
    }

    #[gpui::test]
    fn enter_activates_the_focused_settings_control(cx: &mut TestAppContext) {
        let (view, visual) = cx.add_window_view(|window, cx| {
            let focus_handle = cx.focus_handle();
            let sidebar_toggle_focus = cx.focus_handle().tab_stop(true);
            let settings_open_focus = cx.focus_handle().tab_stop(true);
            let page_focus = PageFocusHandles {
                welcome_open_session: cx.focus_handle().tab_stop(true),
                settings_dark: cx.focus_handle().tab_stop(true),
                settings_light: cx.focus_handle().tab_stop(true),
                settings_home: cx.focus_handle().tab_stop(true),
                session_back: cx.focus_handle().tab_stop(true),
            };
            window.focus(&focus_handle);
            DesktopView {
                state: DesktopState::default(),
                focus_handle,
                sidebar_toggle_focus,
                settings_open_focus,
                page_focus,
            }
        });

        let settings_focus = visual.update(|_, cx| view.read(cx).settings_open_focus.clone());
        visual.update(|window, _| window.focus(&settings_focus));
        visual.run_until_parked();
        visual.update(|window, _| assert!(settings_focus.is_focused(window)));
        visual.simulate_keystrokes("enter");
        visual.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        visual.run_until_parked();

        visual.update(|_, cx| {
            assert_eq!(view.read(cx).state.page, Page::Settings);
        });
        let settings_dark_focus =
            visual.update(|_, cx| view.read(cx).page_focus.settings_dark.clone());
        visual.update(|window, _| assert!(settings_dark_focus.is_focused(window)));
    }

    #[gpui::test]
    fn open_session_and_back_restore_focus_on_each_page(cx: &mut TestAppContext) {
        let (view, visual) = cx.add_window_view(|window, cx| {
            let focus_handle = cx.focus_handle();
            let sidebar_toggle_focus = cx.focus_handle().tab_stop(true);
            let settings_open_focus = cx.focus_handle().tab_stop(true);
            let page_focus = PageFocusHandles {
                welcome_open_session: cx.focus_handle().tab_stop(true),
                settings_dark: cx.focus_handle().tab_stop(true),
                settings_light: cx.focus_handle().tab_stop(true),
                settings_home: cx.focus_handle().tab_stop(true),
                session_back: cx.focus_handle().tab_stop(true),
            };
            window.focus(&focus_handle);
            DesktopView {
                state: DesktopState::default(),
                focus_handle,
                sidebar_toggle_focus,
                settings_open_focus,
                page_focus,
            }
        });

        let welcome_focus =
            visual.update(|_, cx| view.read(cx).page_focus.welcome_open_session.clone());
        visual.update(|window, _| window.focus(&welcome_focus));
        visual.simulate_keystrokes("enter");
        visual.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(view.read(cx).state.page, Page::SessionInformation);
        });

        let back_focus = visual.update(|_, cx| view.read(cx).page_focus.session_back.clone());
        visual.update(|window, _| assert!(back_focus.is_focused(window)));
        visual.simulate_keystrokes("enter");
        visual.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("enter").unwrap(),
        });
        visual.run_until_parked();
        visual.update(|_, cx| {
            assert_eq!(view.read(cx).state.page, Page::Welcome);
        });

        let welcome_focus =
            visual.update(|_, cx| view.read(cx).page_focus.welcome_open_session.clone());
        visual.update(|window, _| assert!(welcome_focus.is_focused(window)));
    }

    #[gpui::test]
    fn tab_reaches_settings_theme_and_return_controls(cx: &mut TestAppContext) {
        let (view, visual) = cx.add_window_view(|window, cx| {
            let focus_handle = cx.focus_handle();
            let sidebar_toggle_focus = cx.focus_handle().tab_stop(true);
            let settings_open_focus = cx.focus_handle().tab_stop(true);
            let page_focus = PageFocusHandles {
                welcome_open_session: cx.focus_handle().tab_stop(true),
                settings_dark: cx.focus_handle().tab_stop(true),
                settings_light: cx.focus_handle().tab_stop(true),
                settings_home: cx.focus_handle().tab_stop(true),
                session_back: cx.focus_handle().tab_stop(true),
            };
            window.focus(&focus_handle);
            DesktopView {
                state: DesktopState {
                    page: Page::Settings,
                    ..DesktopState::default()
                },
                focus_handle,
                sidebar_toggle_focus,
                settings_open_focus,
                page_focus,
            }
        });

        for expected_focus in [
            0, // sidebar toggle
            1, // settings toolbar action
            2, // dark theme
            3, // light theme
            4, // return home
        ] {
            visual.simulate_keystrokes("tab");
            visual.run_until_parked();
            visual.update(|window, cx| {
                let view = view.read(cx);
                let focused = match expected_focus {
                    0 => &view.sidebar_toggle_focus,
                    1 => &view.settings_open_focus,
                    2 => &view.page_focus.settings_dark,
                    3 => &view.page_focus.settings_light,
                    _ => &view.page_focus.settings_home,
                };
                assert!(focused.is_focused(window), "tab stop {expected_focus}");
            });
        }
    }
}
