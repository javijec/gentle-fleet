# Fixtures P0 (capturas anonimizadas)

Capturas reales de los motores instalados, obtenidas con los programas de
`spikes/p0/` y con algunas ejecuciones manuales complementarias. Cada línea de
los `.jsonl` es un objeto:

```json
{"engine": "...", "kind": "...", "payload": { ... }}
```

| Archivo | Motor | Versión | Contenido |
|---|---|---|---|
| `gentle-shell-pi.jsonl` | Gentle Shell → Pi | `4.8.0` / `1.5.2` | `chat`, `permission`, `resume`, `disconnect` (probe) |
| `codex.jsonl` | Codex app-server | `0.171.0` | initialize, thread, turn, aprobación, read, resume (probe) |
| `opencode.jsonl` | OpenCode v2 API | `2.3.2` | chat, `once`, `reject`, persistencia, attach (probe) |
| `codex-missing-auth.jsonl` | Codex app-server | `0.171.0` | `CODEX_HOME` sin `auth.json`: 401, reintentos y turno `failed` (reproducción dirigida) |
| `codex-manual-success.jsonl` | Codex app-server | `0.171.0` | ejecución manual anterior: `waitingOnApproval`, estados MCP, rate limits |
| `opencode-orphan-permission.jsonl` | OpenCode v2 API | `2.3.2` | `permission.asked` de una sesión abandonada vista en otra conexión (manual) |
| `command-outputs.txt` | los tres | ver arriba | salida anonimizada de `--version`, `serve --help` y `app-server --help` |
| `codex-schema-summary.json` | Codex app-server | `0.171.0` | recuento de definiciones v2 y definiciones seleccionadas del esquema generado |
| `opencode-openapi-subset.json` | OpenCode v2 API | `2.3.2` | subconjunto de `GET /openapi.json` con las rutas y esquemas citados en la investigación |

Las capturas se generaron el 20/11/2026. No contienen credenciales: los scripts
nunca leen ni imprimen secretos. Antes de guardarlas,
`spikes/p0/common.py` sustituye `HOME`, hostname, UUIDs e IDs del proveedor por
marcadores (`<HOME>`, `<host>`, `<uuid>`, `<id>`) y recorta cadenas largas a 1200
caracteres; los prompts de sistema de los agentes no se conservan. Los IDs que
aparecen son opacos y no deben interpretarse como IDs Fleet.

`command-outputs.txt` se anonimizó con la misma regla. El password de OpenCode
nunca se escribió en ninguna captura: solo se usó como cabecera HTTP.

Estas capturas son evidencia de forma de mensaje, orden de eventos y campos
disponibles. No son un contrato congelado: las versiones están fijadas y un
cambio de versión obliga a repetir el spike. El detalle de qué afirmación se
apoya en qué captura está en [`docs/research/`](../../../docs/research/).

## Preparación para publicación

Las capturas de Codex conservan la estructura y el orden de los eventos, pero los valores de plan, consumo, reinicio de cuotas y créditos se sustituyeron por ejemplos sintéticos. No son evidencia de la cuenta ni de sus límites reales. Las futuras capturas requieren revisión manual y escaneo de secretos antes de incorporarse: la anonimización de P0 no garantiza eliminar todos los datos personales ni credenciales presentes en mensajes de terceros.
