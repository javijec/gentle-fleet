# P0 — resumen ejecutivo e índice de investigación (20/11/2026)

Estado: **P0 ejecutado con motores reales; cierre definitivo pendiente de la
ratificación de P0-D1…D6**. No se ha implementado P1, daemon, API Fleet ni
esquema definitivo.

Este documento mantenía antes el detalle completo de los tres motores. Ese
detalle vive ahora en [`docs/research/`](research/), con etiquetas de evidencia
(**VERIFIED / OBSERVED / UNTESTED / UNSUPPORTED**) y referencias a script y
captura por afirmación. Aquí solo quedan el resumen, las consecuencias y el
criterio de cierre.

## Versiones y método

| Motor | Versión exacta observada | Superficie |
|---|---|---|
| Gentle Shell | `gentle-shell 4.8.0`, `pi 1.5.2` (paquete `gentle-pi 4.8.0-main.652552becbb6`) | `--mode rpc`, JSONL por stdio |
| Codex | `codex-cli 0.171.0` | `codex app-server`, JSON-RPC 2.0 por stdio |
| OpenCode | `opencode v2.3.2` | `opencode serve`, HTTP + SSE en loopback |

Fuente: [`spikes/p0/fixtures/command-outputs.txt`](../spikes/p0/fixtures/command-outputs.txt).
Los tres probes (`spikes/p0/pi`, `spikes/p0/codex`, `spikes/p0/opencode`)
terminan en `PASS` con esas versiones en Linux x86_64. Son opt-in: no corren en
CI. Usan el login nativo de la máquina sin leer ni guardar credenciales; el
modelo gratuito de OpenCode permitió el smoke sin credencial.

## Documentación de investigación

| Documento | Contenido |
|---|---|
| [`research/PI.md`](research/PI.md) | activation, RPC, UI requests de permisos, resume, muerte al cerrar stdin |
| [`research/CODEX.md`](research/CODEX.md) | handshake, threads/turns/items, aprobaciones servidor→cliente, resume, 401 por `CODEX_HOME` sin auth |
| [`research/OPENCODE.md`](research/OPENCODE.md) | servidor y attach, SSE, permisos REST, persistencia, store global |
| [`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md) | matriz de capacidades, contratos comunes de `AgentDriver`, propuestas P0-D1…D6 |
| [`research/KNOWN_LIMITATIONS.md`](research/KNOWN_LIMITATIONS.md) | fallos reproducidos, no soportado y no investigado |
| [`spikes/p0/README.md`](../spikes/p0/README.md) | cómo ejecutar los probes y índice de fixtures |

## Consecuencias para P1 (resumen)

1. Pi no sobrevive a la desconexión del cliente: exige Runner dueño del proceso.
2. Codex depende de la versión y de credenciales en `CODEX_HOME`; paginar desde el inicio.
3. OpenCode puede adjuntarse al servicio de usuario, persistente y con API tipada;
   por eso es el candidato recomendado para el primer driver real (P0-D1,
   **pendiente de ratificación**).
4. Ningún motor ofrece un cursor de replay comparable; el seq Fleet debe ser propio (P0-D6).

Las propuestas P0-D1…D6, con justificación, alternativas y consecuencias, están
en [`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md#propuestas-p0-d1d6--pendientes-de-ratificación).
No cambian D01–D12 de [`DECISIONS.md`](DECISIONS.md); solo las concretan.

## Criterio de cierre de P0

| Criterio de `PLAN.md` | Resultado |
|---|---|
| Spikes de los tres motores en versiones fijadas | ✅ tres programas ejecutables con `PASS` |
| Fixtures de chat, herramientas, permisos y recuperación | ✅ capturas anonimizadas en `spikes/p0/fixtures/` |
| Documentación comprobada | ✅ este resumen + `docs/research/` + README de spikes |
| Sin credenciales en fixtures | ✅ verificado por anonimización e inspección |
| Sin servicio persistente ni esquema definitivo | ✅ no se creó ninguno |

Queda explícitamente fuera de P0: Windows, SIGKILL del daemon, cancelación real,
replay desde cursor y paginación avanzada. Están clasificados en
[`KNOWN_LIMITATIONS.md`](research/KNOWN_LIMITATIONS.md).
