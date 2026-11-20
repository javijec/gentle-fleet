# Fixtures P0 (capturas anonimizadas)

Tres capturas reales de los motores instalados, obtenidas con los programas de
`spikes/p0/`. Cada línea es un objeto:

```json
{"engine": "...", "kind": "...", "payload": { ... }}
```

| Archivo | Motor | Versión observada | Escenarios capturados |
|---|---|---|---|
| `gentle-shell-pi.jsonl` | Gentle Shell → Pi RPC | gentle-shell 4.8.0-main.652552becbb6 / pi 1.5.2 | `chat`, `permission`, `resume`, `disconnect` |
| `codex.jsonl` | Codex app-server | codex-cli 0.171.0 | `thread/start`, `turn/start`, aprobación, `thread/read`, `thread/resume` |
| `opencode.jsonl` | OpenCode v2 HTTP API | 2.3.2 | `chat`, `permission once`, `permission reject`, `restart persistence`, `attach` |

Las capturas se generaron el 20/11/2026 con modelos reales. No contienen
credenciales: los scripts nunca leen ni imprimen secretos. Antes de guardarlas,
`spikes/p0/common.py` sustituye el directorio `HOME`, el hostname, UUIDs e IDs
del proveedor por marcadores (`<HOME>`, `<host>`, `<uuid>`, `<id>`) y recorta
cadenas largas a 1200 caracteres (los prompts de sistema de los agentes no se
conservan). Los IDs que aparecen son opacos y no deben interpretarse como IDs
Fleet.

Estas capturas son evidencia de forma de mensaje, orden de eventos y campos
disponibles. No son un contrato congelado: las versiones están fijadas y un
cambio de versión obliga a repetir el spike.
