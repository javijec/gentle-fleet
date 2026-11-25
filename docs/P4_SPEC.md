# P4 — Especificación funcional y decisiones aprobadas (Gentle Fleet v2)

**Estado:** alcance de producto aprobado; investigación técnica P4.0 pendiente.  
**Plataforma de referencia:** Linux x86_64 hasta finalizar P6.  
**Rama de trabajo:** `feature/p4-multi-provider`.  
**Carácter de este documento:** registro duradero de decisiones tomadas antes de implementar P4. No equivale a garantía de que un protocolo o capacidad esté disponible.

## 1. Objetivo

P4 integra cuatro proveedores **seleccionables e independientes** en un solo núcleo Fleet:

1. OpenCode (driver existente, referencia y suite de regresión).
2. Codex (App Server estructurado cuando la versión instalada lo soporte).
3. Pi estándar (Pi RPC).
4. Gentle Shell (proveedor propio, con integración Pi RPC y funcionalidades RPC adicionales verificadas).

El producto debe permitir conversaciones reales, herramientas, permisos, sesiones, historial, controles nativos y recuperación según las capacidades de cada proveedor. No se admite una falsa paridad: lo no soportado se declara explícitamente.

## 2. Decisiones de producto ratificadas

| ID | Decisión aprobada |
|---|---|
| P4-F01 | OpenCode, Codex, Pi y Gentle Shell se muestran como **cuatro proveedores independientes**. |
| P4-F02 | Gentle Shell puede reutilizar componentes técnicos de Pi, pero no es un alias ni un simple perfil de Pi. |
| P4-F03 | Para Gentle Shell, P4 cubre **chat y controles nativos de sesión completos dentro de las capacidades comprobadas**. |
| P4-F04 | Las demás funciones de RPC ampliado se investigan y documentan; se dejan puntos de extensión tipados para fases futuras, sin implementar indiscriminadamente todas las funciones. |
| P4-F05 | **Toda gestión/observación/control de subagentes y delegación multiagente** queda fuera de P4 y se estudiará después de P6, incluso si Gentle Shell ya expone RPC para ello. |
| P4-F06 | Se respetan las configuraciones, extensiones, credenciales, sesiones, modelos y perfiles nativos de cada proveedor. Fleet no reescribe ni copia credenciales. |
| P4-F07 | Las políticas de permisos siguen siendo **nativas**; Fleet las consulta/expone y nunca autoaprueba. |
| P4-F08 | Las aprobaciones usan **API Fleet común con decisiones y argumentos nativos tipados**, sin reducirlas artificialmente a once/always/reject ni enviar JSON privado sin contrato. |
| P4-F09 | Modelo y esfuerzo de razonamiento pueden cambiar **durante una sesión si el proveedor lo permite**; los cambios no deben aplicarse silenciosamente a turnos activos. |
| P4-F10 | Se permite descubrir **todas las sesiones nativas accesibles del usuario** que un proveedor pueda enumerar con seguridad, no solo las de workspaces registrados. |
| P4-F11 | Adoptar sesiones externas requiere una acción **explícita** del usuario, evidencias suficientes de seguridad y un **runner propio de Fleet**. No tomar control de procesos externos activos. |
| P4-F12 | Si una sesión externa sigue activa bajo otro controlador, **se bloquea su adopción**. Si no puede determinarse su disponibilidad con seguridad, no se presupone que está libre. |
| P4-F13 | Recuperar **todo el historial estructurado realmente disponible**, distinguiendo eventos históricos importados/reconstruidos de los observados en vivo. No inventar deltas ni detalles ausentes. |
| P4-F14 | Controles nativos de sesión incluidos cuando existan: modelo, esfuerzo, modos, permisos y perfiles. No crear editor general de archivos de configuración. |
| P4-F15 | Se trabaja en cortes **P4.0–P4.4**, con commits, pruebas y validación por corte. |
| P4-F16 | Linux x86_64 es plataforma de validación funcional hasta P6; Windows/macOS se validarán en P7. No declarar compatibilidad no probada; conservar diseño portable. |
| P4-F17 | Gentle Fleet es independiente de **Gentle Shell GPUI**: no tomar su UI, arquitectura, diseño, módulos ni implementación como referencia. Investigar el protocolo en el productor y documentación oficial. |
| P4-F18 | No usar Herdr como dependencia de ejecución. Fleet mantiene su propia autoridad de sesiones, runners y timeline. |

## 3. Límites de arquitectura que deben respetarse

- `SessionManager` es la autoridad para estados de sesión/turno, aprobaciones y operaciones idempotentes; **no introduce ramas por proveedor**.
- `fleet-runner` mantiene el ciclo de vida y conexión al proceso/servicio nativo, además de spool y ACK durables; `fleetd` no necesita importar un protocolo nativo.
- `fleet-domain` y la Fleet API son independientes de Codex/Pi/Gentle Shell/OpenCode; los IDs nativos permanecen opacos.
- `AgentDriver` conserva un núcleo pequeño. Capacidades especializadas deben resolverse en P4.0 con contratos concretos, sin crear traits o crates vacíos ni nuevos subsistemas por anticipación.
- Los eventos son tipados, ordenados por `seq` Fleet, persistidos antes de ACK/publicación y deduplicados. No se filtra RPC nativo crudo a la API de cliente.
- Las desconexiones del cliente no detienen sesiones. Los turnos inciertos **no** se reenvían automáticamente y las aprobaciones **no** se responden por defecto.
- Mantener independencia de cualquier interfaz gráfica: P6 desarrolla el SDK para consumidores externos, no una interfaz propia (alcance actualizado el 25/11/2026); la gestión general de subagentes es posterior a P6.
- No introducir SSH, remoto, SDK de escritorio completo, editor genérico de configuración, worktrees, sistema de plugins, filesystem remoto ni ACP genérico dentro del alcance de P4 sin una nueva decisión.

## 4. Alcance operativo común

Cada driver debe anunciar, por versión/configuración real, su disponibilidad y capacidades: iniciar, enviar prompt, streaming/mensajes completos, herramientas, cancelación, stop, aprobaciones, historial, controles de modelo/esfuerzo/modos, reanudación e inspección de recuperación.

Una capacidad faltante responde con error tipado `UnsupportedCapability` o equivalente. No debe fingirse soporte para producir una matriz verde.

### 4.1 Controles de modelos y perfiles

- Consultar modelos, niveles de esfuerzo, modos y perfiles realmente disponibles.
- Cambiar el estado nativo únicamente mediante operaciones soportadas y con confirmación.
- Durante turnos activos, aplicar solo si es seguro y está documentado; de lo contrario rechazar o diferir explícitamente con estado observable.
- Respetar la configuración del usuario; no modificar directamente ficheros de configuración/credenciales por conveniencia.

### 4.2 Aprobaciones

- Preservar opciones admitidas por cada solicitud, incluso decisiones parametrizadas o UI interactiva específica.
- Fleet conserva la autorización, la idempotencia y el estado durable; el adaptador traduce IDs y payloads nativos.
- Nunca autoaprobar. Entrega incierta exige reconciliación, no reintento automático.

### 4.3 Sesiones externas

**Descubrimiento amplio, adopción conservadora.**

1. Enumerar de forma no intrusiva todas las sesiones accesibles que el proveedor pueda descubrir.
2. Inspeccionar identidad nativa, workspace, estado, historial y evidencia de actividad (sin controlar el proceso).
3. Bloquear la adopción de sesiones activas externamente o de disponibilidad no verificable; la mera ausencia de un PID no acredita inactividad.
4. Con petición explícita, reservar vínculo único entre proveedor + identidad nativa + sesión Fleet y crear runner propio.
5. Importar historial de forma idempotente, con procedencia clara: observado en vivo, importado del proveedor o reconstruido desde evidencias parciales.
6. Permitir nuevo prompt solo tras confirmación de continuación segura; historial recuperable **no** implica que un turno en vuelo pueda reanudarse.

No introducir un mecanismo para tomar posesión de procesos ajenos activos. Una sesión externa no puede tener dos administradores Fleet con escritura.

## 5. Contratos técnicos por investigar en P4.0 (propuestas, no diseño ratificado)

Áreas a comparar con código real existente:

- Descubrimiento de proveedor y negociación de capacidades/versiones.
- Enumeración de sesiones nativas sin efectos secundarios.
- Adopción, exclusividad, vínculo durable, autorización y reintentos idempotentes.
- Importación de historial, procedencia, IDs nativos opacos y deduplicación.
- Cambio de modelo, esfuerzo, modo y perfil.
- Aprobaciones con opciones tipadas/parametrizadas y respuestas.
- Evidencia para reattach de runner vs resume de historial vs recuperación de turno activo.
- Gentle Shell: separación de transporte Pi RPC, Extension UI, y llamadas adicionales de Desktop RPC sin heredar arquitectura de GPUI.

**No** se aprueban por este documento nombres definitivos de traits/crates, nuevas tablas SQL, nuevas operaciones wire ni migraciones. En P4.0, justificar cada cambio con un caso de uso probado y comparar alternativas.

## 6. Plan incremental y criterios de cierre

### P4.0 — Investigación y contratos (sin implementar drivers)

- Leer `AGENTS.md`, `PLAN.md`, `docs/ARCHITECTURE.md`, `docs/API.md`, `docs/DRIVERS.md`, `docs/DECISIONS.md`, `docs/ROADMAP.md`, `docs/research/` y las pruebas/código de P1–P3.
- Detectar versiones instaladas reales de Codex, Pi y Gentle Shell; actualizar investigación P0 si difieren.
- Confirmar protocolos desde los productores, esquemas oficiales y pruebas exploratorias. Gentle Shell Desktop RPC fue investigado solo parcialmente en P0; **no asumir** inventario ni compatibilidad completos.
- Producir matriz verificable por proveedor y versión; diseñar cambios mínimos en contratos y pruebas de regresión antes de modificar `AgentDriver`.
- Documentar decisiones aprobadas frente a hipótesis técnicas pendientes, riesgos y propuestas que requieran autorización.
- Entregables propuestos: `docs/P4_PROVIDER_MATRIX.md`, `docs/P4_RPC_RESEARCH.md`, `docs/P4_CONTRACT_DESIGN.md`, `docs/P4_TEST_PLAN.md`. Este archivo es el registro funcional de referencia.

### P4.1 — Codex

- Usar App Server estructurado según versión real.
- Sesiones/threads, turnos, streaming, herramientas, aprobaciones tipadas, controles admitidos, historial y recuperación verificada.
- Validación con Codex instalado y suites deterministas; nunca confundir `thread/resume` de historial con recuperación de turno en vuelo.

### P4.2 — Pi estándar

- Pi RPC estructurado: proceso propio, stdin/stdout, eventos, fin de trabajo (`agent_settled` cuando corresponda), herramientas, interrupción, stop, historial y controles comprobados.
- Independiente de las extensiones específicas de Gentle Shell.

### P4.3 — Gentle Shell

- Proveedor propio con transporte compartido Pi cuando corresponda y adaptación específica de Desktop RPC.
- Chat y controles nativos acordados, incluida negociación de métodos/capacidades y manejo de funciones ausentes.
- No implementar observación/control de subagentes en P4, aunque se documenten sus métodos.
- No tomar Gentle Shell GPUI como referencia de arquitectura o implementación.

### P4.4 — Conformidad, estabilización y entrega

- Suite de conformidad parametrizada por capacidades para cuatro proveedores.
- Casos reales: conversación, herramienta, permiso (si existe), modelo/esfuerzo (si existe), stop, historial, adopción segura, reinicio de `fleetd`, reattach, replay, dos clientes, concurrencia entre proveedores, idempotencia y errores.
- Reejecutar regresiones completas P1–P3, con OpenCode como línea base.
- Linux x86_64: CI y smokes reales con versiones identificadas. Los smokes opt-in no se sustituyen por FakeDriver.
- Documentar soportado/parcial/no soportado/no probado por proveedor y versión; no cerrar P4 con éxitos ficticios.

## 7. Estrategia de entrega

- Rama `feature/p4-multi-provider`, partiendo de `main` con P3 integrado.
- Commits descriptivos y push por corte funcional; nunca merge automático a `main` ni force push.
- Cada corte puede cerrarse sin dejar al siguiente a medio implementar.
- Validar `cargo fmt`, `cargo check`, `cargo clippy -D warnings`, `cargo test`, `cargo build`, `git diff --check` y E2E correspondientes.
- No publicar datos personales, tokens, perfiles, historiales o credenciales en fixtures/logs.

## 8. Decisiones técnicas aún abiertas (P4.0)

Las siguientes **no son dudas de producto ya respondidas**, sino descubrimientos técnicos:

1. Qué métodos/eventos/versiones existen realmente en el Gentle Shell instalado, especialmente `gentle-shell.desktop-rpc/v1`.
2. Qué componentes RPC puede compartir Pi con Gentle Shell sin acoplamiento indebido.
3. Qué extensiones concretas requiere el código actual de Fleet para operaciones de control y adopción.
4. Qué evidencia de exclusividad proporciona cada proveedor para adoptar sesiones históricas sin pisar actividad externa.
5. Qué historial estructurado y qué recuperación de proceso/turno ofrece cada proveedor.
6. Qué cambios de modelo, esfuerzo, permisos, modos y perfiles son seguros durante un turno.
7. Qué subconjunto de RPC ampliado conviene tipar de inmediato sin implementar sus funcionalidades diferidas.

No resolver esos puntos por suposición; investigar, probar y documentar antes de P4.1.

## 9. Definición final de terminado de P4

Se considera P4 lista para revisión de integración a `main` cuando los cuatro proveedores pueden operar conversaciones reales bajo Fleet en Linux, con los controles y eventos que sus versiones efectivamente soporten, permisos seguros, cumplimiento del ciclo de vida P3, y regresiones de OpenCode en verde.

El descubrimiento externo debe cubrir los repositorios nativos accesibles que el proveedor permita enumerar; la adopción con escritura solo se habilita donde la exclusividad y continuación segura estén demostradas. La ausencia de capacidad es un resultado explícito, no un fallo oculto.

Las limitaciones se publican con evidencia concreta y alcance por versión. Windows/macOS siguen pendientes de validación funcional en P7.

---

**Fuente de autoridad de producto:** decisiones explícitas consensuadas antes de P4.0. Ante contradicción entre este registro y documentación antigua de P4, **no cambiar la decisión por cuenta propia**: señalar la contradicción y actualizar los documentos después de la revisión.
