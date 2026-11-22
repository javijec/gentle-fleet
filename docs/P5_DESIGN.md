# P5 — Diseño del transporte remoto SSH

**Rama:** `feature/p5-ssh-transport`. **Estado:** implementación funcional en Linux; ver `P5_RESULTS.md`.

## 1. Objetivo y decisiones

Una persona usa la misma Fleet API en local o en otra máquina mediante SSH. No existe una segunda API, ni sincronización de bases, ni un listener TCP público.

| ID | Decisión |
|---|---|
| P5-D1 | El bridge SSH es un binario Fleet (`fleet-ssh-bridge`) ejecutado por SSH en el host remoto; transporta frames JSONL existentes. |
| P5-D2 | SSH se invoca con `Command` y argumentos (sin shell, sin PTY, `BatchMode=yes`, `StrictHostKeyChecking=yes`). |
| P5-D3 | El token local NUNCA viaja por SSH: el bridge reescribe `Request::Hello` con el token leído en el host remoto. |
| P5-D4 | `fleetctl` usa `FleetReader`/`FleetWriter`;local y remoto comparten correlación, eventos, errores y timeouts. |
| P5-D5 | El alias local es configuración de cliente; la identidad autoritativa del host es la clave SSH verificada en `known_hosts`. |
| P5-D6 | Una desconexión SSH cierra solo el bridge; `fleetd`, runners y proveedores continúan. |
| P5-D7 | Un prompt ambiguo no se reenvía automáticamente: se recomienda al usuario repetir con el mismo `operation_key` tras reconectar. |
| P5-D8 | `session_id` no cambia: el contexto del host se mantiene en la conexión/alias elegido explícitamente con `--host`. |

## 2. Topología

```text
fleetctl --host alias
   │  FleetReader/FleetWriter (Frame sin cambios)
   ▼
ssh -T -o StrictHostKeyChecking=yes dest fleet-ssh-bridge
   │  stdin/stdout JSONL (stderr separado)
   ▼
fleet-ssh-bridge (en el host remoto)
   │  Hello con token remoto; socket Unix local
   ▼
fleetd → SessionManager → SQLite → fleet-runner → proveedor
```

## 3. Bridge

- Dos hilos: stdin→socket (parsea frames y reescribe `Hello`) y socket→stdout (reenvía verbatim).
- Buffers fijos de 16 KiB; la escritura bloqueante aplica backpressure sin colas ilimitadas.
- Half-close: EOF en stdin ejecuta `shutdown(Write)` en el socket; las respuestas pendientes siguen fluyendo.
- El bridge no envía señales a `fleetd` ni a runners. Al terminar SSH, el daemon permanece.

## 4. Configuración de hosts

`FLEET_SSH_CONFIG` (por defecto `~/.config/gentle-fleet/hosts.json`) contiene alias, destino SSH, ruta absoluta del bridge, puerto, identidad y huella de host opcional. OpenSSH gestiona claves, ProxyJump y autenticación; Fleet no almacena passwords ni claves privadas.

Comandos: `fleetctl host add|list|show|remove|check`. Selección: `fleetctl --host <alias> <comando>`; sin `--host` se mantiene el comportamiento local.

## 5. Errores

El cliente distingue: SSH no disponible, host desconocido, autenticación/identidad rechazada, bridge ausente, daemon remoto apagado, protocolo incompatible, desconexión, timeout y error real del proveedor. La última línea de stderr del bridge se incluye en el error cuando SSH cierra; nunca se devuelve una lista vacía como éxito.

## 6. Seguridad

Sin `StrictHostKeyChecking=no`, sin contraseñas, sin shell interpolation, sin exposición del wire nativo de los proveedores. Los tokens de runner siguen en ficheros `0600` del host remoto.

## 7. Fuera de alcance

PTY, VFS remoto, sincronización de archivos, GPUI, plugins, colaboración multiusuario y compatibilidad Windows/macOS (P7).
