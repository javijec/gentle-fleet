use super::*;
use crate::machines::machine_key;
use gpui::{KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};

fn build_view(
    window: &mut Window,
    cx: &mut Context<DesktopView>,
    state: DesktopState,
    fleet: FleetUiState,
) -> DesktopView {
    let focus_handle = cx.focus_handle();
    let sidebar_toggle_focus = cx.focus_handle().tab_stop(true);
    let settings_open_focus = cx.focus_handle().tab_stop(true);
    let page_focus = PageFocusHandles::new(cx);
    window.focus(&focus_handle);
    DesktopView {
        state,
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
}

fn add_view(
    cx: &mut TestAppContext,
    state: DesktopState,
    fleet: FleetUiState,
) -> (gpui::Entity<DesktopView>, &mut VisualTestContext) {
    cx.add_window_view(|window, cx| build_view(window, cx, state, fleet))
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is rendered"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    visual.run_until_parked();
}

fn assert_constrained_snapshot(
    visual: &mut VisualTestContext,
    view: &gpui::Entity<DesktopView>,
    id: &MachineId,
    expected_session: &str,
) {
    visual.update(|_, cx| {
        let view = view.read(cx);
        let row = view.fleet.row(id).expect("machine row exists");
        assert_eq!(row.status, ConnectionStatus::Connected);
        let snapshot = row.snapshot.as_ref().expect("connected snapshot");
        assert!(
            snapshot
                .agents
                .iter()
                .any(|agent| agent.id == "fake"
                    && agent.availability.eq_ignore_ascii_case("available")),
            "FakeDriver must be discoverable"
        );
        assert!(
            snapshot
                .sessions
                .iter()
                .any(|session| session.id == expected_session),
            "fixture session {expected_session} must be listed"
        );
        assert!(!snapshot.workspaces.is_empty());
    });
}

#[gpui::test]
fn tab_moves_focus_between_the_primary_toolbar_controls(cx: &mut TestAppContext) {
    let (view, visual) = add_view(cx, DesktopState::default(), FleetUiState::default());

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
    let (view, visual) = add_view(cx, DesktopState::default(), FleetUiState::default());

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
    let settings_dark_focus = visual.update(|_, cx| view.read(cx).page_focus.settings_dark.clone());
    visual.update(|window, _| assert!(settings_dark_focus.is_focused(window)));
}

#[gpui::test]
fn open_session_and_back_restore_focus_on_each_page(cx: &mut TestAppContext) {
    let (view, visual) = add_view(cx, DesktopState::default(), FleetUiState::default());

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
    let (view, visual) = add_view(
        cx,
        DesktopState {
            page: Page::Settings,
            ..DesktopState::default()
        },
        FleetUiState::default(),
    );

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
    let mut fleet = FleetUiState::default();
    fleet
        .machines
        .push(MachineRow::new(fleet::MachineCatalogEntry {
            id: MachineId::Ssh("missing-test-alias".into()),
            kind: fleet::MachineKind::Ssh,
        }));
    let (view, visual) = add_view(cx, DesktopState::default(), fleet);

    click(visual, "machine-select-ssh-missing-test-alias");
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
    click(visual, "machine-connect");
    visual.update(|_, cx| {
        let row = view
            .read(cx)
            .fleet
            .row(&MachineId::Ssh("missing-test-alias".into()))
            .unwrap();
        assert_eq!(row.status, ConnectionStatus::Error);
        assert!(row.error.is_some());
        assert!(row.snapshot.is_none());
    });
}

#[gpui::test]
fn returning_home_shows_welcome_even_with_a_selected_machine(cx: &mut TestAppContext) {
    let fleet = FleetUiState {
        selected: Some(MachineId::Local),
        machines: vec![MachineRow::new(fleet::MachineCatalogEntry {
            id: MachineId::Local,
            kind: fleet::MachineKind::Local,
        })],
        ..FleetUiState::default()
    };
    let (view, visual) = add_view(
        cx,
        DesktopState {
            page: Page::Settings,
            ..DesktopState::default()
        },
        fleet,
    );

    click(visual, "settings-home");
    visual.update(|_, cx| {
        let view = view.read(cx);
        assert_eq!(view.state.page, Page::Welcome);
        assert_eq!(view.fleet.selected, None);
    });
    assert!(
        visual.debug_bounds("welcome-open-session").is_some(),
        "welcome actions are visible after returning home"
    );
}

#[test]
fn element_keys_are_stable_and_independent_of_list_position() {
    let local = MachineId::Local;
    let ssh = MachineId::Ssh("build-box".into());
    assert_eq!(machine_key(&local), "local");
    assert_eq!(machine_key(&ssh), "ssh-build-box");
    assert_eq!(
        crate::machines::session_key(&ssh, "ses_1"),
        "ssh-build-box:ses_1"
    );
    assert_ne!(
        crate::machines::session_key(&local, "ses_1"),
        crate::machines::session_key(&ssh, "ses_1")
    );
}

/// Opt-in black-box test driven through the GPUI view against a real isolated
/// `fleetd` fixture (local IPC and a real OpenSSH fixture host).
///
/// Run through `tests/e2e_p62b_desktop_gpui.sh`; it prepares the environment.
#[gpui::test]
#[ignore = "requires the isolated fleetd/sshd fixture from tests/e2e_p62b_desktop_gpui.sh"]
fn real_local_and_ssh_fleet_from_gpui(cx: &mut TestAppContext) {
    let local_session = std::env::var("FLEET_DESKTOP_EXPECT_LOCAL_SESSION")
        .expect("fixture must provide FLEET_DESKTOP_EXPECT_LOCAL_SESSION");
    let ssh_alias = std::env::var("FLEET_DESKTOP_SSH_ALIAS")
        .expect("fixture must provide FLEET_DESKTOP_SSH_ALIAS");
    let ssh_session = std::env::var("FLEET_DESKTOP_EXPECT_SSH_SESSION")
        .expect("fixture must provide FLEET_DESKTOP_EXPECT_SSH_SESSION");
    let ssh_machine = MachineId::Ssh(ssh_alias.clone());

    let (view, visual) = add_view(cx, DesktopState::default(), FleetUiState::default());
    visual.update(|_, cx| {
        view.update(cx, |view, cx| view.refresh_catalog(cx));
    });
    visual.run_until_parked();

    visual.update(|_, cx| {
        let view = view.read(cx);
        assert!(
            view.fleet.row(&MachineId::Local).is_some(),
            "Local must be in the real catalog"
        );
        assert!(
            view.fleet.row(&ssh_machine).is_some(),
            "the configured SSH fixture alias must be in the catalog"
        );
        assert!(view.fleet.catalog_error.is_none());
    });

    // Local: select, connect, inspect data, refresh, disconnect, retry.
    click(visual, "machine-select-local");
    click(visual, "machine-connect");
    assert_constrained_snapshot(visual, &view, &MachineId::Local, &local_session);

    click(visual, "machine-refresh");
    assert_constrained_snapshot(visual, &view, &MachineId::Local, &local_session);

    click(visual, "machine-disconnect");
    visual.update(|_, cx| {
        let row = view.read(cx).fleet.row(&MachineId::Local).unwrap();
        assert_eq!(row.status, ConnectionStatus::Disconnected);
        assert!(
            row.snapshot.is_none(),
            "a disconnected machine must not keep showing its old snapshot"
        );
    });

    click(visual, "machine-connect");
    assert_constrained_snapshot(visual, &view, &MachineId::Local, &local_session);

    // SSH fixture: the same GPUI path must work over the real SSH transport.
    visual.update(|_, cx| {
        view.update(cx, |view, cx| view.select_machine(ssh_machine.clone(), cx));
    });
    visual.run_until_parked();
    click(visual, "machine-connect");
    assert_constrained_snapshot(visual, &view, &ssh_machine, &ssh_session);

    click(visual, "machine-disconnect");
    visual.update(|_, cx| {
        let row = view.read(cx).fleet.row(&ssh_machine).unwrap();
        assert_eq!(row.status, ConnectionStatus::Disconnected);
        assert!(row.snapshot.is_none());
    });
}
