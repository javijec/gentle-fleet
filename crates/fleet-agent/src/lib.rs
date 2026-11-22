//! Contrato `AgentDriver` y registro de proveedores.
//!
//! El SessionManager solo conoce este módulo: nunca importa un driver concreto.
//! Los identificadores nativos se conservan dentro del driver y jamás aparecen
//! en `AgentEvent` ni en el wire Fleet.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod fake;

use fleet_domain::{
    ProviderDescriptor, ProviderId, ProviderVersion, RunnerMode, SessionState, TurnId, VersionRange,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    pub code: String,
    pub message: String,
}

impl AgentError {
    pub fn unsupported(what: &str) -> Self {
        Self {
            code: "unsupported_capability".into(),
            message: format!("{what} is not supported by this driver version"),
        }
    }
}

/// Quita URLs y IDs nativos de mensajes que van a convertirse en eventos
/// públicos. Los mensajes de reqwest incluyen la URL completa, que contiene
/// `ses_*`, `per_*` y otros identificadores opacos del proveedor.
pub fn redact_provider_error(message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut chars = message.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_whitespace() {
            output.push(ch);
            continue;
        }
        let token_start = output.len();
        output.push(ch);
        while let Some(&next) = chars.peek() {
            if next.is_ascii_whitespace() {
                break;
            }
            output.push(next);
            chars.next();
        }
        let token = &output[token_start..];
        if token.starts_with("http://") || token.starts_with("https://") {
            let replacement = "[url redacted]";
            output.truncate(token_start);
            output.push_str(replacement);
        }
    }
    let mut redacted = output;
    for prefix in ["ses_", "msg_", "per_", "call_", "exec_", "evt_", "sh_"] {
        let mut result = String::with_capacity(redacted.len());
        let mut rest = redacted.as_str();
        while let Some(index) = rest.find(prefix) {
            result.push_str(&rest[..index]);
            let after = &rest[index + prefix.len()..];
            let end = after
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .unwrap_or(after.len());
            result.push_str(prefix);
            result.push_str("<id>");
            rest = &after[end..];
        }
        result.push_str(rest);
        redacted = result;
    }
    // Los diagnósticos del proveedor pueden incluir tokens y rutas de
    // credenciales aunque no contengan una URL ni un ID nativo.
    let mut safe = String::with_capacity(redacted.len());
    let mut token = String::new();
    let mut after_bearer = false;
    let flush = |safe: &mut String, token: &mut String, after_bearer: &mut bool| {
        if token.is_empty() {
            return;
        }
        let raw = token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | ':' | ';'));
        let credential_file = raw.contains("auth.json")
            || raw.contains("service.json")
            || raw.contains("/.codex/")
            || raw.contains("/.config/opencode/");
        let credential_token = raw.starts_with("sk-") && raw.len() >= 15
            || raw.starts_with("ghp_") && raw.len() >= 16
            || raw.starts_with("github_pat_")
            || raw.starts_with("sk-proj-");
        if *after_bearer || credential_token || credential_file {
            safe.push_str("[credential redacted]");
        } else {
            safe.push_str(token);
        }
        *after_bearer = raw.eq_ignore_ascii_case("bearer");
        token.clear();
    };
    for ch in redacted.chars() {
        if ch.is_whitespace() {
            flush(&mut safe, &mut token, &mut after_bearer);
            safe.push(ch);
        } else {
            token.push(ch);
        }
    }
    flush(&mut safe, &mut token, &mut after_bearer);
    safe
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchContext {
    pub workspace: PathBuf,
    pub mode: RunnerMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInput {
    pub text: String,
    pub turn_id: TurnId,
    /// Controles deseados para ESTE turno. No se aplican silenciosamente a un
    /// turno ya en curso (P4-F09).
    #[serde(default)]
    pub controls: Vec<fleet_domain::RequestedControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendReceipt {
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryEvidence {
    pub session_alive: bool,
    pub detail: String,
}

pub use fleet_domain::{AgentEvent, DomainEvent, RunnerEvent};

/// Contrato mínimo de P1. Las operaciones no soportadas devuelven
/// `AgentError::unsupported`, nunca un fallo silencioso.
pub trait AgentDriver: Send {
    fn descriptor(&self) -> &ProviderDescriptor;

    fn open(&mut self, ctx: LaunchContext) -> Result<String, AgentError>;

    fn send(&mut self, input: AgentInput) -> Result<SendReceipt, AgentError>;

    /// Devuelve `None` cuando no hay eventos listos dentro del timeout.
    fn next_event(&mut self, timeout: Duration) -> Result<Option<AgentEvent>, AgentError>;

    fn close(&mut self) -> Result<(), AgentError>;

    fn inspect_recovery(&self) -> RecoveryEvidence;

    fn answer(&mut self, _request_id: String, _decision: String) -> Result<(), AgentError> {
        Err(AgentError::unsupported("interactive approvals (P2)"))
    }

    /// P4.1-D: entrega opciones tipadas. El default conserva el contrato P1–P3
    /// de drivers que solo entienden una decisión como cadena.
    fn answer_with_args(
        &mut self,
        request_id: String,
        decision: String,
        _args: Vec<fleet_domain::ApprovalArgument>,
    ) -> Result<(), AgentError> {
        self.answer(request_id, decision)
    }

    fn interrupt(&mut self, _turn_id: TurnId) -> Result<(), AgentError> {
        Err(AgentError::unsupported("cancel_turn (P2/P4)"))
    }

    fn session_state(&self) -> SessionState {
        SessionState::Running
    }

    /// P4.1-E: controles nativos descubiertos. Los drivers que no los tienen
    /// devuelven una lista vacía en lugar de fingir soporte.
    fn list_controls(&self) -> Vec<fleet_domain::ControlState> {
        Vec::new()
    }

    fn get_control(&self, _name: &str) -> Result<fleet_domain::ControlState, AgentError> {
        Err(AgentError::unsupported("native controls"))
    }

    /// `scope` es `turn` o `session`. El estado devuelto indica si aplicó ya
    /// o si quedó diferido al siguiente turno (`deferred`).
    fn set_control(
        &mut self,
        _name: &str,
        _value: &str,
        _scope: &str,
    ) -> Result<fleet_domain::ControlState, AgentError> {
        Err(AgentError::unsupported("native controls"))
    }
}

pub use fleet_domain::{NativeHistoryMessage, NativeSessionInfo};

/// Fábrica registrada por proveedor. `compatibility` aplica el rango de
/// versiones sin bloquear actualizaciones compatibles (P0-D5).
pub trait DriverFactory: Send + Sync {
    fn id(&self) -> ProviderId;
    fn compatibility(&self) -> VersionRange;
    fn create(&self) -> Box<dyn AgentDriver>;

    /// Metadatos estáticos. La versión puede ser desconocida hasta `probe`.
    fn metadata(&self) -> ProviderDescriptor;

    /// Comprobación barata de disponibilidad/versión; no abre sesión.
    fn probe(&self) -> ProviderDescriptor {
        self.metadata()
    }

    fn supports(&self, version: &ProviderVersion) -> bool {
        self.compatibility().compatible(version)
    }

    /// P4.1-F: descubrimiento global de solo lectura a nivel de proveedor.
    /// No crea sesión Fleet ni runner; un proveedor sin este método declara
    /// ausencia de capacidad devolviendo una lista vacía.
    fn list_native_sessions(&self) -> Result<Vec<NativeSessionInfo>, AgentError> {
        Ok(Vec::new())
    }

    fn import_native_history(
        &self,
        _native_ref: &str,
    ) -> Result<Vec<NativeHistoryMessage>, AgentError> {
        Err(AgentError::unsupported("native history import"))
    }
}

#[derive(Default)]
pub struct ProviderRegistry {
    factories: HashMap<ProviderId, Box<dyn DriverFactory>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Punto de extensión: Codex y Pi se registran aquí en P4 sin tocar
    /// SessionManager.
    pub fn register(&mut self, factory: Box<dyn DriverFactory>) {
        self.factories.insert(factory.id(), factory);
    }

    pub fn contains(&self, id: &str) -> bool {
        self.factories.contains_key(id)
    }

    pub fn create(&self, id: &str) -> Result<Box<dyn AgentDriver>, AgentError> {
        match self.factories.get(id) {
            Some(factory) => Ok(factory.create()),
            None => Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{id}`"),
            }),
        }
    }

    pub fn ids(&self) -> Vec<ProviderId> {
        let mut ids: Vec<_> = self.factories.keys().cloned().collect();
        ids.sort();
        ids
    }

    pub fn supports(&self, id: &str, version: &ProviderVersion) -> Result<bool, AgentError> {
        match self.factories.get(id) {
            Some(factory) => Ok(factory.supports(version)),
            None => Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{id}`"),
            }),
        }
    }

    pub fn list_native_sessions(&self, id: &str) -> Result<Vec<NativeSessionInfo>, AgentError> {
        match self.factories.get(id) {
            Some(factory) => factory.list_native_sessions(),
            None => Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{id}`"),
            }),
        }
    }

    pub fn import_native_history(
        &self,
        id: &str,
        native_ref: &str,
    ) -> Result<Vec<NativeHistoryMessage>, AgentError> {
        match self.factories.get(id) {
            Some(factory) => factory.import_native_history(native_ref),
            None => Err(AgentError {
                code: "unknown_provider".into(),
                message: format!("no driver registered for `{id}`"),
            }),
        }
    }

    /// Información real del registro para `ListAgents`.
    pub fn descriptors(&self) -> Vec<ProviderDescriptor> {
        let mut result: Vec<_> = self
            .factories
            .values()
            .map(|factory| factory.probe())
            .collect();
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_do_not_leak_native_ids() {
        let message =
            "POST http://127.0.0.1:49374/api/session/ses_secret/permission/per_secret/reply failed";
        let redacted = redact_provider_error(message);
        assert!(!redacted.contains("ses_secret"));
        assert!(!redacted.contains("per_secret"));
        assert!(!redacted.contains("http://"));
        assert!(redacted.contains("[url redacted]"));
    }

    #[test]
    fn provider_error_redaction_filters_injected_native_ids_keys_and_credential_paths() {
        // Fixtures falsos: no incorporar secretos de sesiones personales.
        let message = "provider failed: ses_deadbeef0011223344 \
            sk-CONFORMANCEFAKETOKEN123 Bearer ABCDEFGHIJKLMNOPQ \
            /home/fixture/.codex/auth.json /home/fixture/.config/opencode/service.json";
        let safe = redact_provider_error(message);
        for secret in [
            "deadbeef0011223344",
            "sk-CONFORMANCEFAKETOKEN123",
            "ABCDEFGHIJKLMNOPQ",
            "auth.json",
            "service.json",
            "/home/fixture",
        ] {
            assert!(!safe.contains(secret), "leaked fixture: {secret}");
        }
        assert!(safe.contains("[credential redacted]"));
        assert!(safe.contains("ses_<id>"));
    }

    #[test]
    fn unknown_provider_is_reported() {
        let registry = ProviderRegistry::new();
        match registry.create("codex") {
            Ok(_) => panic!("unknown provider must fail"),
            Err(err) => assert_eq!(err.code, "unknown_provider"),
        }
    }
}
