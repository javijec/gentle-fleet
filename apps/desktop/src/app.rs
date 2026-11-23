use crate::{
    components::{primary_button, quiet_button},
    fleet::{
        self, Cancellation, ConnectionStatus, FleetUiState, FleetWorkers, MachineId, MachineRow,
        SafeFleetError, SessionIdentity,
    },
    navigation::Page,
    state::DesktopState,
    theme::{FONT_SIZE_BODY, FONT_SIZE_TITLE, SPACING},
};
use gpui::{
    div, prelude::*, px, size, App, Application, Bounds, ClickEvent, Context, FocusHandle,
    KeyDownEvent, Render, Window, WindowBounds, WindowOptions,
};

struct DesktopView {
    state: DesktopState,
    fleet: FleetUiState,
    active_client: Option<(MachineId, fleet::Connection)>,
    cancellation: Cancellation,
    workers: FleetWorkers,
    selected_session: Option<SessionIdentity>,
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

    fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
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

    fn select_machine(&mut self, id: MachineId, cx: &mut Context<Self>) {
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
                row.status = ConnectionStatus::Disconnected;
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
                    row.status = ConnectionStatus::Disconnected;
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

    fn connect_machine(&mut self, id: MachineId, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        if let Some(previous_id) = self.fleet.active_request_machine.take() {
            if previous_id != id {
                if let Some(row) = self.fleet.row_mut(&previous_id) {
                    if matches!(
                        row.status,
                        ConnectionStatus::Connecting | ConnectionStatus::Reconnecting
                    ) {
                        row.status = ConnectionStatus::Disconnected;
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
                row.error = Some(SafeFleetError {
                    kind: fleet::FleetErrorKind::Other,
                    message: "Hay dos consultas Fleet en curso; espera antes de reintentar.",
                });
                row.status = ConnectionStatus::Error;
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
                    row.status = ConnectionStatus::Disconnected;
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
                        if let Some(row) = view.fleet.row_mut(&id) {
                            row.status = fleet::status_for_error(&id, &error);
                            row.error = Some(error);
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

    fn disconnect_machine(&mut self, id: MachineId, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.fleet.request_generation = self.fleet.request_generation.wrapping_add(1);
        if let Some(pending_id) = self.fleet.active_request_machine.take() {
            if let Some(row) = self.fleet.row_mut(&pending_id) {
                row.status = ConnectionStatus::Disconnected;
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
            row.status = ConnectionStatus::Disconnected;
            row.error = None;
        }
        cx.notify();
    }

    fn cancel_connection(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        self.cancellation.cancel();
        self.fleet.request_generation = self.fleet.request_generation.wrapping_add(1);
        if self.fleet.active_request_machine.as_ref() == Some(id) {
            self.fleet.active_request_machine = None;
        }
        if let Some(row) = self.fleet.row_mut(id) {
            row.status = ConnectionStatus::Disconnected;
            row.error = None;
        }
        cx.notify();
    }

    fn toggle_machine(&mut self, id: &MachineId, cx: &mut Context<Self>) {
        if let Some(row) = self.fleet.row_mut(id) {
            row.expanded = !row.expanded;
        }
        cx.notify();
    }

    fn select_session(&mut self, identity: SessionIdentity, cx: &mut Context<Self>) {
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
        let page = if self.fleet.selected.is_some() && state.page != Page::Settings {
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

fn status_label(status: &ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Disconnected => "Desconectada",
        ConnectionStatus::Connecting => "Conectando…",
        ConnectionStatus::Connected => "Conectada",
        ConnectionStatus::Reconnecting => "Reconectando…",
        ConnectionStatus::Error => "Error",
        ConnectionStatus::Unavailable => "No disponible",
    }
}

fn render_machines_sidebar(
    view: &DesktopView,
    palette: crate::theme::Palette,
    cx: &mut Context<DesktopView>,
) -> impl IntoElement {
    let rows = view.fleet.machines.clone();
    let selected = view.fleet.selected.clone();
    div()
        .id("machines-sidebar")
        .w(px(f32::from(view.state.sidebar_width)))
        .h_full()
        .flex()
        .flex_col()
        .gap_3()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(palette.border)
        .bg(palette.panel)
        .p_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_xs().text_color(palette.muted).child("MACHINES"))
                .child(quiet_button(
                    "machines-refresh",
                    "Actualizar",
                    palette,
                    None,
                    cx.listener(|view, _, _, cx| view.refresh_catalog(cx)),
                )),
        )
        .when_some(view.fleet.catalog_error.as_ref(), |this, error| {
            this.child(
                div()
                    .text_sm()
                    .text_color(palette.muted)
                    .child(error.message),
            )
        })
        .children(rows.into_iter().enumerate().map(|(row_index, row)| {
            let id = row.entry.id.clone();
            let select_id = id.clone();
            let toggle_id = id.clone();
            let is_selected = selected.as_ref() == Some(&id);
            let mut machine = div()
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .rounded(px(8.0))
                .border_1()
                .border_color(if is_selected {
                    palette.focus
                } else {
                    palette.border
                })
                .bg(if is_selected {
                    palette.elevated
                } else {
                    palette.panel
                })
                .p_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .id(("machine-expand", row_index))
                                .focusable()
                                .tab_stop(true)
                                .cursor_pointer()
                                .child(if row.expanded { "▾" } else { "▸" })
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.toggle_machine(&toggle_id, cx)
                                })),
                        )
                        .child(
                            div()
                                .id(("machine-select", row_index))
                                .focusable()
                                .tab_stop(true)
                                .debug_selector(|| "machine-select-row".to_string())
                                .flex_1()
                                .cursor_pointer()
                                .child(format!(
                                    "{}  {}",
                                    row.entry.id.label(),
                                    status_label(&row.status)
                                ))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.select_machine(select_id.clone(), cx)
                                })),
                        ),
                );
            if row.expanded {
                if let Some(snapshot) = row.snapshot {
                    machine =
                        machine.child(div().pl_5().text_xs().text_color(palette.muted).child(
                            format!(
                                "{} proveedores · {} workspaces · {} sesiones",
                                snapshot.agents.len(),
                                snapshot.workspaces.len(),
                                snapshot.sessions.len()
                            ),
                        ));
                    for agent in snapshot.agents {
                        let availability = if agent.availability.eq_ignore_ascii_case("available") {
                            "●"
                        } else {
                            "○"
                        };
                        machine = machine.child(
                            div()
                                .pl_5()
                                .text_sm()
                                .text_color(palette.muted)
                                .child(format!("{availability} {}", agent.id)),
                        );
                    }
                    for (session_index, session) in snapshot.sessions.into_iter().enumerate() {
                        let session_identity = SessionIdentity {
                            machine: row.entry.id.clone(),
                            session_id: session.id.clone(),
                        };
                        machine = machine.child(
                            div()
                                .id(("sidebar-session", session_index))
                                .focusable()
                                .tab_stop(true)
                                .pl_5()
                                .text_xs()
                                .text_color(palette.muted)
                                .cursor_pointer()
                                .child(format!("↳ {}", session.id))
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.select_session(session_identity.clone(), cx)
                                })),
                        );
                    }
                } else if let Some(error) = row.error {
                    machine = machine.child(
                        div()
                            .pl_5()
                            .text_xs()
                            .text_color(palette.muted)
                            .child(error.message),
                    );
                } else if row.status == ConnectionStatus::Disconnected {
                    machine = machine.child(
                        div()
                            .pl_5()
                            .text_xs()
                            .text_color(palette.muted)
                            .child("Desconectada · selecciona para conectar"),
                    );
                }
            }
            machine
        }))
}

fn render_machine_detail(
    view: &DesktopView,
    palette: crate::theme::Palette,
    cx: &mut Context<DesktopView>,
) -> impl IntoElement {
    let id = view
        .fleet
        .selected
        .clone()
        .expect("selected machine exists");
    let row = view.fleet.row(&id);
    let machine_name = id.label().to_string();
    let status = row
        .map(|row| status_label(&row.status))
        .unwrap_or("Desconocida");
    let kind = match row.map(|row| row.entry.kind) {
        Some(fleet::MachineKind::Local) => "Local",
        Some(fleet::MachineKind::Ssh) => "SSH",
        None => "Desconocido",
    };
    let snapshot = row.and_then(|row| row.snapshot.clone());
    let error = row.and_then(|row| row.error.clone());
    let selected_session = view.selected_session.clone();
    let connect_id = id.clone();

    let mut detail = div()
        .id("machine-detail")
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(SPACING[2]))
        .overflow_y_scroll()
        .p_8()
        .child(
            div()
                .text_size(px(FONT_SIZE_TITLE))
                .font_weight(gpui::FontWeight::BOLD)
                .child(machine_name.clone()),
        )
        .child(
            div()
                .text_color(palette.muted)
                .child(format!("{kind} · {status}")),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .when(
                    row.is_some_and(|row| {
                        matches!(
                            row.status,
                            ConnectionStatus::Disconnected
                                | ConnectionStatus::Error
                                | ConnectionStatus::Unavailable
                        )
                    }),
                    |this| {
                        this.child(quiet_button(
                            "machine-connect",
                            if row.is_some_and(|row| {
                                matches!(
                                    row.status,
                                    ConnectionStatus::Error | ConnectionStatus::Unavailable
                                )
                            }) {
                                "Reintentar conexión"
                            } else {
                                "Conectar"
                            },
                            palette,
                            None,
                            cx.listener(move |view, _, _, cx| {
                                view.connect_machine(connect_id.clone(), cx)
                            }),
                        ))
                    },
                )
                .when(
                    row.is_some_and(|row| {
                        matches!(
                            row.status,
                            ConnectionStatus::Connecting | ConnectionStatus::Reconnecting
                        )
                    }),
                    |this| {
                        let cancel_id = id.clone();
                        this.child(quiet_button(
                            "machine-cancel",
                            "Cancelar consulta",
                            palette,
                            None,
                            cx.listener(move |view, _, _, cx| {
                                view.cancel_connection(&cancel_id, cx)
                            }),
                        ))
                    },
                )
                .when(
                    row.is_some_and(|row| row.status == ConnectionStatus::Connected),
                    |this| {
                        let refresh_id = id.clone();
                        let disconnect_id = id.clone();
                        this.child(quiet_button(
                            "machine-refresh",
                            "Actualizar",
                            palette,
                            None,
                            cx.listener(move |view, _, _, cx| {
                                view.connect_machine(refresh_id.clone(), cx)
                            }),
                        ))
                        .child(quiet_button(
                            "machine-disconnect",
                            "Desconectar",
                            palette,
                            None,
                            cx.listener(move |view, _, _, cx| {
                                view.disconnect_machine(disconnect_id.clone(), cx)
                            }),
                        ))
                    },
                ),
        );

    if let Some(error) = error {
        detail = detail.child(
            div()
                .rounded(px(8.0))
                .border_1()
                .border_color(palette.border)
                .p_3()
                .text_color(palette.muted)
                .child(error.message),
        );
    }
    if let Some(snapshot) = snapshot {
        detail = detail
            .child(div().text_size(px(FONT_SIZE_BODY)).child("Proveedores"))
            .children(snapshot.agents.iter().map(|agent| {
                div().text_color(palette.muted).child(format!(
                    "{} · {}{}",
                    agent.id,
                    agent.availability,
                    agent
                        .version
                        .as_ref()
                        .map(|version| format!(" · {version}"))
                        .unwrap_or_default()
                ))
            }))
            .child(div().text_size(px(FONT_SIZE_BODY)).child("Workspaces"))
            .children(snapshot.workspaces.iter().map(|workspace| {
                div()
                    .text_color(palette.muted)
                    .child(format!("{} · {}", workspace.id, workspace.path))
            }))
            .child(div().text_size(px(FONT_SIZE_BODY)).child("Sesiones"))
            .children(
                snapshot
                    .sessions
                    .iter()
                    .enumerate()
                    .map(|(index, session)| {
                        let identity = SessionIdentity {
                            machine: id.clone(),
                            session_id: session.id.clone(),
                        };
                        let selected = selected_session.as_ref() == Some(&identity);
                        div()
                            .id(("detail-session", index))
                            .focusable()
                            .tab_stop(true)
                            .cursor_pointer()
                            .rounded(px(6.0))
                            .bg(if selected {
                                palette.elevated
                            } else {
                                palette.canvas
                            })
                            .p_2()
                            .text_color(palette.muted)
                            .child(format!(
                                "{} · {} · workspace {} · {}",
                                session.id,
                                session.provider_id,
                                session.workspace_id,
                                session.state
                            ))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.select_session(identity.clone(), cx)
                            }))
                    }),
            )
            .when(snapshot.sessions.is_empty(), |this| {
                this.child(
                    div()
                        .text_color(palette.muted)
                        .child("Esta máquina no tiene sesiones Fleet."),
                )
            });
    }
    if let Some(identity) = selected_session.filter(|session| session.machine == id) {
        if let Some(session) = row
            .and_then(|row| row.snapshot.as_ref())
            .and_then(|snapshot| {
                snapshot
                    .sessions
                    .iter()
                    .find(|session| session.id == identity.session_id)
            })
        {
            detail = detail
                .child(
                    div()
                        .text_size(px(FONT_SIZE_BODY))
                        .child("Sesión seleccionada"),
                )
                .child(div().text_color(palette.muted).child(format!(
                    "{} · {} · {} · {}",
                    session.id, session.provider_id, session.workspace_id, session.state
                )))
                .child(
                    div()
                        .text_color(palette.muted)
                        .child("Abrir chat estará disponible en P6.2-C."),
                );
        }
    }
    detail
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
mod tests {
    use super::*;
    use gpui::{KeyUpEvent, Keystroke, Modifiers, TestAppContext};

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
                fleet: FleetUiState::default(),
                active_client: None,
                cancellation: Cancellation::default(),
                workers: FleetWorkers::default(),
                selected_session: None,
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
                fleet: FleetUiState::default(),
                active_client: None,
                cancellation: Cancellation::default(),
                workers: FleetWorkers::default(),
                selected_session: None,
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
                fleet: FleetUiState::default(),
                active_client: None,
                cancellation: Cancellation::default(),
                workers: FleetWorkers::default(),
                selected_session: None,
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
                fleet: FleetUiState::default(),
                active_client: None,
                cancellation: Cancellation::default(),
                workers: FleetWorkers::default(),
                selected_session: None,
                focus_handle,
                sidebar_toggle_focus,
                settings_open_focus,
                page_focus,
            }
        });

        let mut visited_settings_controls = [false; 3];
        for _ in 0..12 {
            visual.simulate_keystrokes("tab");
            visual.run_until_parked();
            visual.update(|window, cx| {
                let view = view.read(cx);
                visited_settings_controls[0] |= view.page_focus.settings_dark.is_focused(window);
                visited_settings_controls[1] |= view.page_focus.settings_light.is_focused(window);
                visited_settings_controls[2] |= view.page_focus.settings_home.is_focused(window);
            });
        }
        assert!(visited_settings_controls.into_iter().all(|visited| visited));
    }

    #[gpui::test]
    fn machines_panel_selects_a_host_without_blocking_render(cx: &mut TestAppContext) {
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
            let mut fleet = FleetUiState::default();
            fleet
                .machines
                .push(MachineRow::new(fleet::MachineCatalogEntry {
                    id: MachineId::Ssh("missing-test-alias".into()),
                    kind: fleet::MachineKind::Ssh,
                }));
            DesktopView {
                state: DesktopState::default(),
                fleet,
                active_client: None,
                cancellation: Cancellation::default(),
                workers: FleetWorkers::default(),
                selected_session: None,
                focus_handle,
                sidebar_toggle_focus,
                settings_open_focus,
                page_focus,
            }
        });

        let bounds = visual
            .debug_bounds("machine-select-row")
            .expect("machine row is rendered");
        visual.simulate_click(bounds.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(
                view.fleet.selected,
                Some(MachineId::Ssh("missing-test-alias".into()))
            );
            let row = view
                .fleet
                .row(&MachineId::Ssh("missing-test-alias".into()))
                .unwrap();
            assert_eq!(row.status, ConnectionStatus::Disconnected);
        });
        let connect_bounds = visual
            .debug_bounds("machine-connect")
            .expect("manual connect action is rendered");
        visual.simulate_click(connect_bounds.center(), Modifiers::default());
        visual.run_until_parked();
        visual.update(|_, cx| {
            let row = view
                .read(cx)
                .fleet
                .row(&MachineId::Ssh("missing-test-alias".into()))
                .unwrap();
            assert_eq!(row.status, ConnectionStatus::Error);
            assert!(row.error.is_some());
        });
    }
}
