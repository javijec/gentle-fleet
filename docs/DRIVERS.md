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

Primer driver candidato: gentle-shell --mode rpc (según la implementación y versión instalada durante P0). P0 debe capturar: handshake, mensaje de prompt, fragmentos de salida, solicitudes select/confirm/input/editor, resolución/cancelación, errores, cierre, persistencia y recursos. Registrar versiones y fixtures reales sin credenciales.

No asumir que Pi devuelve recibos idempotentes de prompt o resume entre procesos: inspeccionar y probar. Si no soporta recuperación exacta del turno, Fleet reporta OutcomeUnknown y no reenvía.

## Codex

Preferir su app-server estructurado (JSONL/JSON-RPC, threads/turns/items, server-initiated approval requests). El driver traduce threadId→NativeSessionId, turnId→NativeTurnId e ítems a mensajes/herramientas Fleet, sin convertir los IDs nativos en IDs Fleet. Respetar las decisiones permitidas y políticas de aprobación del servidor. La compatibilidad se verifica contra la versión real y fixtures de App Server.

Referencia: https://learn.chatgpt.com/docs/app-server

## OpenCode

Priorizar su API/servidor soportado por la versión instalada, no analizar salida TUI. Detectar si el proveedor dispone ya de un servidor por usuario y evitar iniciar instancias duplicadas sin necesidad. Mapear session IDs, eventos, prompts, permissions e historial a la misma interfaz AgentDriver. El proceso dueño real de la sesión podría ser el propio servidor OpenCode; la entidad Runner maneja el attach, no debe suponer un hijo exclusivamente suyo.

Referencias: https://opencode.ai/docs/cli y https://opencode.ai/docs/api

## ACP y fallback

ACP es una ruta de expansión posterior, si la versión instalada de un agente implementa los comandos necesarios. Se añade un driver ACP genérico una vez que dos integraciones nativas compartan los mismos tests. Un agente CLI sin API estructurada puede operar como terminal con UX y permisos limitados, indicados claramente; no se le da una etiqueta de chat completo.

## Suite de conformidad por proveedor

A cada driver se le aplican los mismos tests de contrato: discover, start en cwd real, un prompt con texto, un turno completo, evento de herramienta si hay soporte, una solicitud interactive si hay soporte, cierre, reattach si hay soporte, errores, cancelación y limpieza. Dos capas: fixtures offline deterministas + smoke real opt-in etiquetado con el binario/versión. Las pruebas son matrix-driven por capacidades y no asumen paridad falsa.

## Selección de implementación

P0 mide qué driver puede completar el primer flujo con menos traducción y sin hacks. Elegir uno para P1 **por evidencia, no por familiaridad**. Los otros dos se implementan en P4 reutilizando pruebas de contrato, sin tocar SessionManager ni el wire Fleet.
