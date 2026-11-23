use crate::{
    components::quiet_button,
    fleet::{
        self, Cancellation, ConnectionStatus, FleetUiState, FleetWorkers, MachineId, MachineRow,
        SafeFleetError, SessionIdentity,
    },
    machines::{render_machine_detail, render_machines_sidebar},
    navigation::Page,
    pages::{render_page, PageFocusHandles},
    state::DesktopState,
    theme::FONT_SIZE_BODY,
};
use gpui::{
    div, prelude::*, px, size, App, Application, Bounds, ClickEvent, Context, FocusHandle,
    KeyDownEvent, Render, Window, WindowBounds, WindowOptions,
};

pub(crate) struct DesktopView {
    pub(crate) state: DesktopState,
    pub(crate) fleet: FleetUiState,
    pub(crate) active_client: Option<(MachineId, fleet::Connection)>,
    pub(crate) cancellation: Cancellation,
    pub(crate) workers: FleetWorkers,
    pub(crate) selected_session: Option<SessionIdentity>,
    pub(crate) focus_handle: FocusHandle,
    pub(crate) sidebar_toggle_focus: FocusHandle,
    pub(crate) settings_open_focus: FocusHandle,
    pub(crate) page_focus: PageFocusHandles,
}

impl DesktopView {
    fn open_settings(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.state.navigate(Page::Settings);
        window.focus(&self.page_focus.settings_dark);
        cx.notify();
    }

    pub(crate) fn show_welcome(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Returning home must show the welcome page even when a machine was
        // selected before. The live connection is retained and can be revisited
        // from the Machines panel.
        self.fleet.selected = None;
        self.selected_session = None;
        self.state.navigate(Page::Welcome);
        window.focus(&self.page_focus.welcome_open_session);
        cx.notify();
    }

    pub(crate) fn show_session_information(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.navigate(Page::SessionInformation);
        window.focus(&self.page_focus.session_back);
        cx.notify();
    }

    pub(crate) fn select_dark_theme(
        &mut self,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.set_theme(crate::theme::ThemeMode::Dark);
        cx.notify();
    }

    pub(crate) fn select_light_theme(
        &mut self,
        _: &ClickEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.set_theme(crate::theme::ThemeMode::Light);
        cx.notify();
    }

    pub(crate) fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(permit) = self.workers.try_acquire() else {
            self.fleet.catalog_error = Some(SafeFleetError {
                kind: fleet::FleetErrorKind::Other,
                message: "Hay dos operaciones Fleet en curso; espera antes de actualizar.",
            });
            cx.notify();
            return;
        };
        let generation = self.fleet.catalog_generation.wrapping_add(1);
        self.fleet.catalog_generation = generation;
        let task = cx.background_executor().spawn(async move {
            let _permit = permit;
            fleet::discover_catalog()
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if view.fleet.catalog_generation != generation {
                    return;
                }
                match result {
                    Ok(catalog) => {
                        let selected = view.fleet.selected.clone();
                        let mut rows: Vec<_> = catalog.into_iter().map(MachineRow::new).collect();
                        let old_rows = std::mem::take(&mut view.fleet.machines);
                        for row in &mut rows {
                            if let Some(old) =
                                old_rows.iter().find(|old| old.entry.id == row.entry.id)
                            {
                                row.expanded = old.expanded;
                                row.snapshot = old.snapshot.clone();
                                row.status = old.status.clone();
                                row.error = old.error.clone();
                            }
                        }
                        if view
                            .active_client
                            .as_ref()
                            .is_some_and(|(host, _)| !rows.iter().any(|row| &row.entry.id == host))
                        {
                            if let Some((_, mut client)) = view.active_client.take() {
                                cx.background_executor()
                                    .spawn(async move { client.disconnect() })
                                    .detach();
                            }
                        }
                        view.fleet.machines = rows;
                        view.fleet.selected = selected
                            .filter(|id| view.fleet.machines.iter().any(|row| &row.entry.id == id));
                        view.fleet.catalog_error = None;
                    }
                    Err(error) => {
                        if !view
                            .fleet
                            .machines
                            .iter()
                            .any(|row| row.entry.id == MachineId::Local)
                        {
                            view.fleet.machines.insert(
                                0,
                                MachineRow::new(fleet::MachineCatalogEntry {
                                    id: MachineId::Local,
                                    kind: fleet::MachineKind::Local,
                                }),
                            );
                        }
                        view.fleet.catalog_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn select_machine(&mut self, id: MachineId, cx: &mut Context<Self>) {
        self.selected_session = None;
        self.switch_selection(id, cx);
    }

    fn switch_selection(&mut self, id: MachineId, cx: &mut Context<Self>) {
        if self.fleet.active_request_machine.as_ref() == Some(&id) {
            self.fleet.selected = Some(id);
            cx.notify();
            return;
        }

        if let Some(previous_id) = self.fleet.active_request_machine.take() {
            self.cancellation.cancel();
            self.fleet.request_generation = self.fleet.request_generation.wrapping_add(1);
            if let Some(row) = self.fleet.row_mut(&previous_id) {
                row.mark_disconnected();
            }
        }
        let active_same_machine = self
            .active_client
            .as_ref()
            .is_some_and(|(active_id, _)| active_id == &id);
        if let Some((previous_id, mut client)) = self.active_client.take() {
            if previous_id == id {
                self.active_client = Some((previous_id, client));
            } else {
                if let Some(row) = self.fleet.row_mut(&previous_id) {
                    row.mark_disconnected();
                }
                cx.background_executor()
                    .spawn(async move { client.disconnect() })
                    .detach();
            }
        }
        self.fleet.selected = Some(id.clone());
        if active_same_machine
            && self
                .fleet
                .row(&id)
                .is_some_and(|row| row.status == ConnectionStatus::Connected)
        {
            self.connect_machine(id, cx);
        } else {
            cx.notify();
        }
    }

    pub(crate) fn connect_machine(&mut self, id: MachineId, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        if let Some(previous_id) = self.fleet.active_request_machine.take() {
            if previous_id != id {
                if let Some(row) = self.fleet.row_mut(&previous_id) {
                    if matches!(
                        row.status,
                        ConnectionStatus::Connecting | ConnectionStatus::Reconnecting
                    ) {
                        row.mark_disconnected();
                    }
                }
            }
        }
        self.cancellation = Cancellation::default();
        let cancellation = self.cancellation.clone();
        let generation = self.fleet.request_generation.wrapping_add(1);
        self.fleet.request_generation = generation;
        let Some(permit) = self.workers.try_acquire() else {
            if let Some(row) = self.fleet.row_mut(&id) {
                row.mark_failed(
                    ConnectionStatus::Error,
                    SafeFleetError {
                        kind: fleet::FleetErrorKind::Other,
                        message: "Hay dos consultas Fleet en curso; espera antes de reintentar.",
                    },
                );
            }
            cx.notify();
            return;
        };
        self.fleet.active_request_machine = Some(id.clone());
        if let Some(row) = self.fleet.row_mut(&id) {
            row.status = if row.snapshot.is_some() {
                ConnectionStatus::Reconnecting
            } else {
                ConnectionStatus::Connecting
            };
            row.error = None;
        }

        let reusable_client = self.active_client.take().map(|(previous_id, client)| {
            if previous_id != id {
                if let Some(row) = self.fleet.row_mut(&previous_id) {
                    row.mark_disconnected();
                }
            }
            client
        });
        let request_id = id.clone();
        let task = cx.background_executor().spawn(async move {
            let _permit = permit;
            fleet::inspect(&request_id, &cancellation, reusable_client)
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |view, cx| {
                if !fleet::response_is_current(
                    view.fleet.request_generation,
                    view.fleet.selected.as_ref(),
                    generation,
                    &id,
                ) {
                    if let Ok((_, mut client)) = result {
                        cx.background_executor()
                            .spawn(async move { client.disconnect() })
                            .detach();
                    }
                    return;
                }
                view.fleet.active_request_machine = None;
                match result {
                    Ok((snapshot, client)) => {
                        if let Some(row) = view.fleet.row_mut(&id) {
                            row.status = ConnectionStatus::Connected;
                            row.snapshot = Some(snapshot);
                            row.error = None;
                        }
                        view.active_client = Some((id, client));
                    }
                    Err(error) => {
                        let status = fleet::status_for_error(&id, &error);
                        if let Some(row) = view.fleet.row_mut(&id) {
                            row.mark_failed(status, error);
                        }
                        view.active_client = None;
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn disconnect_machine(&mut self, id: MachineId, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.fleet.request_generation = self.fleet.request_generation.wrapping_add(1);
        if let Some(pending_id) = self.fleet.active_request_machine.take() {
            if let Some(row) = self.fleet.row_mut(&pending_id) {
                row.mark_disconnected();
            }
        }
        if let Some((host, mut client)) = self.active_client.take() {
            if host == id {
                cx.background_executor()
                    .spawn(async move { client.disconnect() })
                    .detach();
            } else {
                self.active_client = Some((host, client));
            }
        }
        if let Some(row) = self.fleet.row_mut(&id) {
            row.mark_disconnected();
        }
        cx.notify();
    }

    pub(crate) fn cancel_connection(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.fleet.request_generation = self.fleet.request_generation.wrapping_add(1);
        if self.fleet.active_request_machine.as_ref() == Some(id) {
            self.fleet.active_request_machine = None;
        }
        if let Some(row) = self.fleet.row_mut(id) {
            row.mark_disconnected();
        }
        cx.notify();
    }

    pub(crate) fn toggle_machine(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        if let Some(row) = self.fleet.row_mut(id) {
            row.expanded = !row.expanded;
        }
        cx.notify();
    }

    pub(crate) fn select_session(&mut self, identity: SessionIdentity, cx: &mut Context<Self>) {
        let machine = identity.machine.clone();
        self.selected_session = Some(identity);
        self.switch_selection(machine, cx);
    }

    fn toggle_sidebar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.state.toggle_sidebar();
        if self.fleet.machines.is_empty() && self.fleet.catalog_error.is_none() {
            self.refresh_catalog(cx);
        }
        cx.notify();
    }
}

impl Drop for DesktopView {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Render for DesktopView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.state.theme.palette();
        let state = &self.state;
        let page_focus = self.page_focus.clone();
        let sidebar = if state.sidebar_open {
            Some(render_machines_sidebar(self, palette, cx))
        } else {
            None
        };
        let page = if self.fleet.selected.is_some() && state.page == Page::Welcome {
            render_machine_detail(self, palette, cx).into_any_element()
        } else {
            render_page(state, palette, cx, page_focus).into_any_element()
        };

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
                    .when_some(sidebar, |this, sidebar| this.child(sidebar))
                    .child(page),
            )
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
                    let page_focus = PageFocusHandles::new(cx);
                    window.focus(&focus_handle);
                    cx.new(|_| DesktopView {
                        state: DesktopState::default(),
                        fleet: FleetUiState::default(),
                        active_client: None,
                        cancellation: Cancellation::default(),
                        workers: FleetWorkers::default(),
                        selected_session: None,
                        focus_handle,
                        sidebar_toggle_focus,
                        settings_open_focus,
                        page_focus,
                    })
                },
            )
            .expect("could not open Gentle Fleet Desktop window");
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus_handle);
                view.refresh_catalog(cx);
            })
            .expect("could not focus Gentle Fleet Desktop window");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests;
