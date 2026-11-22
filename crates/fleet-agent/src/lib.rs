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

/// Sanitiza mensajes que van a convertirse en eventos públicos.
///
/// Se aplica en todos los puntos donde un error, aviso o diagnóstico nativo
/// puede llegar a la API: URLs con credenciales en query, tokens con
/// prefijo, cabeceras `Authorization`, asignaciones `api_key=...`, rutas de
/// credenciales de los cuatro proveedores en Linux/Windows/macOS y IDs
/// nativos opacos. La información no sensible se conserva.
pub fn redact_provider_error(message: &str) -> String {
    let without_urls = redact_urls(message);
    let without_ids = redact_native_ids(&without_urls);
    redact_credentials(&without_ids)
}

fn is_value_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | ',' | ';' | ')' | ']' | '}' | '&' | '?' | '\n' | '\r'
        )
}

fn find_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
        .map(|offset| from + offset)
}

fn starts_with_ci(text: &str, marker: &str) -> bool {
    text.get(..marker.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(marker))
}

fn redact_urls(message: &str) -> String {
    let mut output = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(index) = find_ci(rest, "https://", 0).or_else(|| find_ci(rest, "http://", 0)) {
        output.push_str(&rest[..index]);
        output.push_str("[url redacted]");
        let after = &rest[index..];
        let end = after
            .char_indices()
            .find(|(_, ch)| is_value_delimiter(*ch))
            .map(|(offset, _)| offset)
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    output.push_str(rest);
    output
}

fn redact_native_ids(message: &str) -> String {
    let mut redacted = message.to_string();
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
    redacted
}

/// Redacta tokens y rutas de credenciales preservando la clave diagnóstica
/// (`api_key=[credential redacted]`, `Authorization: [credential redacted]`).
fn redact_credentials(message: &str) -> String {
    const MARKER: &str = "[credential redacted]";
    const KEY_MARKERS: [&str; 8] = [
        "api_key=",
        "api-key=",
        "token=",
        "password=",
        "secret=",
        "client_secret=",
        "client-secret=",
        "authorization=",
    ];
    const TOKEN_PREFIXES: [&str; 9] = [
        "sk-",
        "sk-proj-",
        "ghp_",
        "gho_",
        "ghu_",
        "github_pat_",
        "xoxb-",
        "AIza",
        "ya29.",
    ];
    const PATH_MARKERS: [&str; 16] = [
        "auth.json",
        "service.json",
        "permission-mode.json",
        "/.codex/",
        "/.pi/",
        "/.gentle-shell/",
        "/.config/opencode/",
        "/Users/",
        "/home/",
        r"C:\Users\",
        r"\AppData\",
        r"\.codex\",
        r"\.pi\",
        r"\.gentle-shell\",
        "/root/",
        "/private/var/",
    ];

    fn span_end(value: &str, quoted: bool) -> usize {
        let rest = if quoted && !value.is_empty() {
            &value[1..]
        } else {
            value
        };
        let len = rest.find(is_value_delimiter).unwrap_or(rest.len());
        len + usize::from(quoted && !value.is_empty())
    }

    let mut output = String::with_capacity(message.len());
    let mut index = 0usize;
    while index < message.len() {
        let rest = &message[index..];
        let mut matched: Option<(usize, usize, String)> = None;

        // Authorization:Bearer <token> (sin espacio tras los dos puntos).
        if starts_with_ci(rest, "bearer") {
            let offset = 0usize;
            let after = &rest["bearer".len()..];
            let value_start = after
                .find(|ch: char| !(ch.is_whitespace() || ch == ':' || ch == '='))
                .unwrap_or(after.len());
            let value = &after[value_start..];
            let value_end = value.find(is_value_delimiter).unwrap_or(value.len());
            matched = Some((
                offset,
                offset + "bearer".len() + value_start + value_end,
                MARKER.into(),
            ));
        }

        for marker in KEY_MARKERS {
            if let Some(offset) = find_ci(rest, marker, 0) {
                if matched
                    .as_ref()
                    .is_some_and(|(start, _, _)| *start <= offset)
                {
                    continue;
                }
                let after = &rest[offset + marker.len()..];
                let quoted = after.starts_with('"') || after.starts_with('\'');
                let end = offset + marker.len() + span_end(after, quoted);
                let replacement = format!("{marker}{MARKER}");
                matched = Some((offset, end, replacement));
                break;
            }
        }
        if matched.is_none() {
            for prefix in TOKEN_PREFIXES {
                if starts_with_ci(rest, prefix) {
                    let offset = 0usize;
                    let after = rest;
                    let end = after.find(is_value_delimiter).unwrap_or(after.len());
                    matched = Some((offset, offset + end, MARKER.into()));
                    break;
                }
            }
        }
        if matched.is_none() {
            for marker in PATH_MARKERS {
                if starts_with_ci(rest, marker) {
                    let offset = 0usize;
                    let after = rest;
                    let end = after.find(is_value_delimiter).unwrap_or(after.len());
                    matched = Some((offset, offset + end, MARKER.into()));
                    break;
                }
            }
        }

        if let Some((start, end, replacement)) = matched {
            output.push_str(&rest[..start]);
            output.push_str(&replacement);
            index += end;
        } else {
            let ch = rest.chars().next().expect("non-empty rest");
            output.push(ch);
            index += ch.len_utf8();
        }
    }
    output
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
    fn redaction_covers_inline_keys_headers_urls_paths_and_json() {
        // Todas las credenciales son ficticias y se generan solo para el test.
        let cases = [
            "request failed api_key=sk-FAKEKEY1234567890ABCDEF",
            "request failed api-key: sk-proj-FAKEKEY1234567890",
            "Authorization:Bearer FAKEBEARERTOKEN1234567890",
            "Authorization: Bearer FAKEBEARERTOKEN1234567890",
            "token='ghp_FAKEGITHUBTOKEN1234567890' rejected",
            "(github_pat_FAKEGITHUBPAT1234567890) rejected",
            "GET https://example.test/api?api_key=sk-FAKEQUERYKEY12345 failed",
            "cannot read /home/fixture/.codex/auth.json",
            "cannot read /Users/fixture/.pi/agent/auth.json",
            "cannot read C:\\Users\\fixture\\.gentle-shell\\agent\\auth.json",
            "cannot read /root/.config/opencode/service.json",
            "{\"error\":\"auth failed\",\"api_key\":\"sk-FAKEJSONKEY1234567890\"}",
            "line one ses_deadbeef0011223344\nline two token=sk-FAKEKEY1234567890ABCDEF",
        ];
        let secrets = [
            "sk-FAKEKEY1234567890ABCDEF",
            "sk-proj-FAKEKEY1234567890",
            "FAKEBEARERTOKEN1234567890",
            "ghp_FAKEGITHUBTOKEN1234567890",
            "github_pat_FAKEGITHUBPAT1234567890",
            "sk-FAKEQUERYKEY12345",
            "auth.json",
            "service.json",
            "/home/fixture",
            "/Users/fixture",
            r"C:\Users\fixture",
            "/root/.config/opencode",
            "deadbeef0011223344",
            "sk-FAKEJSONKEY1234567890",
        ];
        for message in cases {
            let safe = redact_provider_error(message);
            for secret in secrets {
                if message.contains(secret) {
                    assert!(
                        !safe.contains(secret),
                        "leaked `{secret}` from `{message}` -> `{safe}`"
                    );
                }
            }
            assert!(
                safe.contains("[credential redacted]")
                    || safe.contains("[url redacted]")
                    || safe.contains("ses_<id>")
            );
        }
    }

    #[test]
    fn redaction_preserves_non_sensitive_diagnostics() {
        let safe = redact_provider_error("permission rejected after 3 retries (exit=17)");
        assert_eq!(safe, "permission rejected after 3 retries (exit=17)");
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
