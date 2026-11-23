use gpui::{div, prelude::*, px, Context};

use crate::{
    app::DesktopView,
    components::quiet_button,
    fleet::{self, ConnectionStatus, MachineId, SessionIdentity},
    theme::{Palette, FONT_SIZE_BODY, FONT_SIZE_TITLE, SPACING},
};

/// Stable element/debug key for a machine, independent of list position.
pub(crate) fn machine_key(id: &MachineId) -> String {
    match id {
        MachineId::Local => "local".to_string(),
        MachineId::Ssh(alias) => format!("ssh-{alias}"),
    }
}

pub(crate) fn session_key(machine: &MachineId, session_id: &str) -> String {
    format!("{}:{session_id}", machine_key(machine))
}

pub(crate) fn status_label(status: &ConnectionStatus) -> &'static str {
    match status {
        ConnectionStatus::Disconnected => "Desconectada",
        ConnectionStatus::Connecting => "Conectando…",
        ConnectionStatus::Connected => "Conectada",
        ConnectionStatus::Reconnecting => "Reconectando…",
        ConnectionStatus::Error => "Error",
        ConnectionStatus::Unavailable => "No disponible",
    }
}

pub(crate) fn render_machines_sidebar(
    view: &DesktopView,
    palette: Palette,
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
        .children(rows.into_iter().map(|row| {
            let id = row.entry.id.clone();
            let key = machine_key(&id);
            let select_id = id.clone();
            let toggle_id = id.clone();
            let select_selector = format!("machine-select-{key}");
            let expand_selector = format!("machine-expand-{key}");
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
                                .id((gpui::ElementId::from("machine-expand"), key.clone()))
                                .focusable()
                                .tab_stop(true)
                                .debug_selector(move || expand_selector)
                                .cursor_pointer()
                                .child(if row.expanded { "▾" } else { "▸" })
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.toggle_machine(&toggle_id, cx)
                                })),
                        )
                        .child(
                            div()
                                .id((gpui::ElementId::from("machine-select"), key.clone()))
                                .focusable()
                                .tab_stop(true)
                                .debug_selector(move || select_selector)
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
                let snapshot = row
                    .snapshot
                    .clone()
                    .filter(|_| row.status == ConnectionStatus::Connected);
                if let Some(snapshot) = snapshot {
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
                    for session in snapshot.sessions {
                        let session_identity = SessionIdentity {
                            machine: row.entry.id.clone(),
                            session_id: session.id.clone(),
                        };
                        let session_selector =
                            format!("session-{}", session_key(&row.entry.id, &session.id));
                        machine = machine.child(
                            div()
                                .id((
                                    gpui::ElementId::from("sidebar-session"),
                                    session_key(&row.entry.id, &session.id),
                                ))
                                .focusable()
                                .tab_stop(true)
                                .debug_selector(move || session_selector)
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

pub(crate) fn render_machine_detail(
    view: &DesktopView,
    palette: Palette,
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
    // Never present data from an earlier connection while the row is not
    // currently connected. `mark_disconnected`/`mark_failed` also clear it.
    let snapshot = row
        .and_then(|row| row.snapshot.clone())
        .filter(|_| row.is_some_and(|row| row.status == ConnectionStatus::Connected));
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
        detail =
            detail
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
                .children(snapshot.sessions.iter().map(|session| {
                    let identity = SessionIdentity {
                        machine: id.clone(),
                        session_id: session.id.clone(),
                    };
                    let selected = selected_session.as_ref() == Some(&identity);
                    let key = session_key(&id, &session.id);
                    let selector = format!("detail-session-{key}");
                    div()
                        .id((gpui::ElementId::from("detail-session"), key))
                        .focusable()
                        .tab_stop(true)
                        .debug_selector(move || selector)
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
                            session.id, session.provider_id, session.workspace_id, session.state
                        ))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.select_session(identity.clone(), cx)
                        }))
                }))
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
