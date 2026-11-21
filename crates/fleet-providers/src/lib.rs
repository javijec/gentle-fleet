//! Catálogo de proveedores incorporados.
//!
//! Esta capa de composición vive FUERA de `fleet-agent`: `fleet-agent`
//! conserva únicamente los contratos (`AgentDriver`, `DriverFactory`,
//! `ProviderRegistry`) y no depende de drivers concretos. `fleetd` y
//! `fleet-runner` comparten este único catálogo, de modo que no puede
//! divergir la lista de proveedores entre el daemon y el runner (P4.1-A.3).

use fleet_agent::fake::{FakeConfig, FakeFactory};
use fleet_agent::ProviderRegistry;
use fleet_driver_codex::CodexFactory;
use fleet_driver_gentle_shell::GentleShellFactory;
use fleet_driver_opencode::OpenCodeFactory;
use fleet_driver_pi::PiFactory;

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok()
}

fn fake_config_from_env() -> FakeConfig {
    FakeConfig {
        block_send: env_flag("FLEET_FAKE_BLOCK_SEND"),
        block_close: env_flag("FLEET_FAKE_BLOCK_CLOSE"),
        fail_send: env_flag("FLEET_FAKE_FAIL_SEND"),
        emit_provider_error: env_flag("FLEET_FAKE_PROVIDER_ERROR"),
        emit_terminal_failure: env_flag("FLEET_FAKE_TERMINAL_FAILURE"),
        emit_approval: env_flag("FLEET_FAKE_APPROVAL"),
        fail_answer: env_flag("FLEET_FAKE_FAIL_ANSWER"),
        fail_interrupt: env_flag("FLEET_FAKE_FAIL_INTERRUPT"),
        interrupt_without_terminal: env_flag("FLEET_FAKE_INTERRUPT_WITHOUT_TERMINAL"),
        defer_terminal_until_interrupt: env_flag("FLEET_FAKE_DEFER_TERMINAL"),
        slow_turn_ms: std::env::var("FLEET_FAKE_SLOW_TURN_MS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
    }
}

/// Registra exactamente el mismo catálogo en cualquier binario que lo llame.
pub fn register_builtin_providers(
    registry: &mut ProviderRegistry,
    fake_config: FakeConfig,
    opencode_ask_shell: bool,
) {
    registry.register(Box::new(FakeFactory::with_config(fake_config)));
    registry.register(Box::new(OpenCodeFactory::with_ask_shell(
        opencode_ask_shell,
    )));
    registry.register(Box::new(CodexFactory));
    registry.register(Box::new(PiFactory));
    registry.register(Box::new(GentleShellFactory));
}

/// Construye el catálogo leyendo las mismas variables de entorno en ambos
/// ejecutables. Es la única función que `fleetd` y `fleet-runner` deben llamar.
pub fn builtin_registry_from_env() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    register_builtin_providers(
        &mut registry,
        fake_config_from_env(),
        env_flag("FLEET_OPENCODE_ASK_SHELL"),
    );
    registry
}

#[cfg(test)]
mod tests {
    use super::*;
    use fleet_domain::RunnerMode;

    #[test]
    fn builtin_catalog_is_stable_and_has_no_duplicate_ids() {
        let a = builtin_registry_from_env();
        let b = builtin_registry_from_env();
        assert_eq!(a.ids(), b.ids());
        let ids = a.ids();
        assert_eq!(ids.len(), 5);
        assert!(ids.contains(&"fake".to_string()));
        assert!(ids.contains(&"opencode".to_string()));
        assert!(ids.contains(&"codex".to_string()));
        assert!(ids.contains(&"pi".to_string()));
        assert!(ids.contains(&"gentle-shell".to_string()));
        let mut seen = std::collections::HashSet::new();
        for descriptor in a.descriptors() {
            assert!(seen.insert(descriptor.id.clone()));
            assert!(matches!(
                descriptor.mode,
                RunnerMode::Owned | RunnerMode::Attached | RunnerMode::Auto
            ));
        }
    }
}
