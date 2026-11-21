# Interfaz de agentes (AgentDriver)

## Objetivo

Los drivers traducen protocolos nativos a eventos de dominio Fleet sin modificar la gestión de sesiones. No se trata de interfaces de terminal: para los primeros tres agentes se privilegia la API nativa estructurada. Un PTY genérico es fallback explícito con capacidades reducidas, no un chat perfecto simulado por análisis de ANSI.

## Interfaz lógica propuesta

~~~rust
// Diseño orientativo: los tipos y firmas se fijan con el primer prototipo.
trait AgentDriver {
    fn provider_id(&self) -> ProviderId;
    fn discover(&self) -> ProviderDiagnostics;
    fn capabilities(&self) -> ProviderCapabilities;
    async fn open(&mut self, ctx: LaunchContext, resume: Option<NativeSessionId>) -> Result<NativeSessionId>;
    async fn send(&mut self, turn_id: TurnId, input: AgentInput) -> Result<SendReceipt>;
    async fn next_event(&mut self) -> Result<Option<AgentEvent>>;
    async fn answer(&mut self, request: NativeRequestId, answer: Decision) -> Result<Ack>;
    async fn interrupt(&mut self, turn: TurnId) -> Result<Ack>;
    async fn close(&mut self) -> Result<()>;
    async fn inspect_recovery(&mut self) -> Result<RecoveryEvidence>;
}
~~~

Este pseudocódigo **no** impone un runtime async, ni una implementación uniforme de resume, ni garantiza que todos los proveedores puedan inspeccionar un turno interrumpido. El contrato real expresará Unsupported para funciones ausentes.

## Capacidades declaradas por agente

supports_resume, streaming_text, tool_events, approvals, cancel_turn, attach_files, image_input, model_switch, modes, session_history, native_turn_ids, native_request_ids, cwd_support. Cada capacidad debe poder desactivarse por versión o configuración. El cliente no deduce funciones por el nombre del proveedor.

## Gentle Shell / Pi

Primer candidato investigado: `gentle-shell --mode rpc`. P0 capturó handshake, disposición del prompt, `message_update`, `tool_execution_*`, solicitudes `select/confirm/input/editor`, resolución, cierre, persistencia y resume con `switch_session`; detalle en [`research/PI.md`](research/PI.md) y captura en `spikes/p0/fixtures/gentle-shell-pi.jsonl`. El cerrar stdin mata al proceso, por lo que este driver exige que el Runner lo posea. Para P1 se recomienda empezar con OpenCode por evidencia (P0-D1, pendiente de ratificación), sin cambiar esta interfaz de driver.

No asumir que Pi devuelve recibos idempotentes de prompt o resume entre procesos: inspeccionar y probar. Si no soporta recuperación exacta del turno, Fleet reporta OutcomeUnknown y no reenvía.

## Codex

Preferir su app-server estructurado (JSONL/JSON-RPC, threads/turns/items, server-initiated approval requests). El driver traduce threadId→NativeSessionId, turnId→NativeTurnId e ítems a mensajes/herramientas Fleet, sin convertir los IDs nativos en IDs Fleet. Respetar las decisiones permitidas y políticas de aprobación del servidor. La compatibilidad se verifica contra la versión real y fixtures de App Server.

P0 confirmó `thread/start`, `turn/start`, aprobaciones como requests JSON-RPC del servidor, `thread/resume` entre procesos y el requisito de paginar con `thread/turns/list` en vez de `thread/read includeTurns`. El esquema generado depende de la versión (714 definiciones v2 en 0.171.0) y aislar `CODEX_HOME` sin `auth.json` rompe el turno con 401. Detalle y evidencia en [`research/CODEX.md`](research/CODEX.md).

Referencia: https://learn.chatgpt.com/docs/app-server

## OpenCode

Priorizar su API/servidor soportado por la versión instalada, no analizar salida TUI. Detectar si el proveedor dispone ya de un servidor por usuario y evitar iniciar instancias duplicadas sin necesidad. Mapear session IDs, eventos, prompts, permissions e historial a la misma interfaz AgentDriver. El proceso dueño real de la sesión podría ser el propio servidor OpenCode; la entidad Runner maneja el attach, no debe suponer un hijo exclusivamente suyo.

P0 confirmó en 2.3.2: attach al servicio de usuario con password de `service.json`, SSE en `/api/event`, `permission.asked` + `POST .../reply` con `once|always|reject`, persistencia tras reinicio y store global que exige filtrar por `location.directory`. Detalle y evidencia en [`research/OPENCODE.md`](research/OPENCODE.md). Es el primer driver real (P0-D1 ratificada). En P2 implementa además la traducción de aprobaciones: `permission.asked` → `ApprovalRequested`, `POST .../reply` para entregar la decisión y `permission.replied` como confirmación. Codex y Pi harán su propia traducción en P4 reutilizando el mismo modelo de dominio.

Referencias: https://opencode.ai/docs/cli y https://opencode.ai/docs/api

## ACP y fallback

ACP es una ruta de expansión posterior, si la versión instalada de un agente implementa los comandos necesarios. Se añade un driver ACP genérico una vez que dos integraciones nativas compartan los mismos tests. Un agente CLI sin API estructurada puede operar como terminal con UX y permisos limitados, indicados claramente; no se le da una etiqueta de chat completo.

## Suite de conformidad por proveedor

A cada driver se le aplican los mismos tests de contrato: discover, start en cwd real, un prompt con texto, un turno completo, evento de herramienta si hay soporte, una solicitud interactive si hay soporte, cierre, reattach si hay soporte, errores, cancelación y limpieza. Dos capas: fixtures offline deterministas + smoke real opt-in etiquetado con el binario/versión. Las pruebas son matrix-driven por capacidades y no asumen paridad falsa.

## Selección de implementación

P0 mide qué driver puede completar el primer flujo con menos traducción y sin hacks. Elegir uno para P1 **por evidencia, no por familiaridad**. Los otros dos se implementan en P4 reutilizando pruebas de contrato, sin tocar SessionManager ni el wire Fleet.

## P4.0 — resultado de la investigación (22/11/2026)

El núcleo de esta interfaz se mantiene: `AgentDriver`, `DriverFactory` y `ProviderRegistry` siguen siendo el punto de extensión y `SessionManager` no gana ramas por proveedor. P4.0 **no ha cambiado firmas ni ha creado crates**. La investigación sí demostró tres carencias que los nuevos drivers encontrarían de inmediato: (1) no hay forma de consultar/cambiar modelo, esfuerzo, modo o perfil; (2) las aprobaciones se reducen a `Vec<String>` + un `decision: String`, insuficiente para las decisiones parametrizadas de Codex; (3) no existe enumeración/adopción de sesiones nativas ni importación de historial con procedencia.

Las propuestas con alternativas, severidad y impacto están en [`P4_CONTRACT_DESIGN.md`](P4_CONTRACT_DESIGN.md); el inventario de capacidades verificado por versión en [`P4_PROVIDER_MATRIX.md`](P4_PROVIDER_MATRIX.md) y el detalle de protocolos en [`P4_RPC_RESEARCH.md`](P4_RPC_RESEARCH.md). Los nombres concretos de traits y operaciones **no están ratificados**: se fijarán en el corte que implemente cada caso de uso probado (P4_SPEC §5 y §98).

Correcciones locales recomendadas antes de P4.1: redactar los IDs nativos en las rutas de `stop` (hoy `fleet-core` publica `error`/`reason` crudos) y conectar el `RunnerCommand::Interrupt` ya existente a `AgentDriver::interrupt`, que hoy es no-op en ambos bucles. Ambas tienen prueba de regresión prevista en [`P4_TEST_PLAN.md`](P4_TEST_PLAN.md).

Nota de versión: los datos P0 de esta sección corresponden a Pi `1.5.2` dentro de Gentle Shell `4.8.0` y Codex `0.171.0`. El `pi` estándar del PATH es **`1.6.0`**; ver matriz P4 antes de atribuir capacidades a "Pi".
