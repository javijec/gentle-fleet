# P7-W2 — instalaciones observadas en Windows

## Descubrimiento Fleet de `0203b5c` (26/11/2026)

`fleetctl agents` consultó un daemon aislado. Para los dos proveedores Node
se configuraron rutas explícitas y `C:\Program Files\nodejs\node.exe`; no se
ejecutaron shims mediante shell. Los entrypoints se comprobaron también con
`node.exe <entrypoint> --version`.

| Proveedor | Resultado Fleet | Ruta / versión observada |
|---|---|---|
| OpenCode | Available, Attached | `C:\Users\javij\.bun\bin\opencode.exe`, 2.4.2 |
| Codex | Available, Owned | `C:\Users\javij\AppData\Local\Programs\OpenAI\Codex\bin\codex.exe`, 0.172.0 |
| Pi | Unavailable, Owned | `C:\Users\javij\.pi\agent\bin\pi-launcher.js`, 1.0.4, fuera del rango |
| Gentle Shell | Available, Owned | `C:\Users\javij\AppData\Roaming\npm\node_modules\gentle-pi\bin\gentle-shell.mjs`, 4.8.0-main.a9d3f5661621; Pi integrado 1.6.0 |

Solo acredita descubrimiento y versión, no apertura, chat o conformidad.
Codex ahora informa 0.172.0; el 0.170.0 de la observación histórica abajo
no debe atribuirse a esta ejecución. No se instaló ni actualizó ningún agente.

Sondeo de `--version` el 26/11/2026. Rutas locales informativas; no se
registraron credenciales. Detectado en PATH no significa disponible mediante
Fleet. Las cuatro comprobaciones de catálogo Fleet y sesiones reales están
**UNTESTED**, al igual que la conformidad.

| Proveedor | Ruta observada | Versión del comando | Observación |
|---|---|---|---|
| OpenCode | `C:\Users\javij\.bun\bin\opencode.exe` | 2.4.2 | EXE; ejecución funcional sin probar |
| Codex | `C:\Users\javij\AppData\Local\Programs\OpenAI\Codex\bin\codex.exe` | 0.170.0 | Inferior al mínimo 0.171 del driver |
| Pi | `C:\Users\javij\.pi\agent\bin\pi.cmd` | 1.0.4 | Shim CMD; inferior al mínimo 1.6 del driver |
| Gentle Shell | `C:\Users\javij\AppData\Roaming\npm\gentle-shell.cmd` | 4.8.0-main.a9d3f5661621 | Shim CMD; reporta Pi 1.6.0 integrado |

## Ejecución conjunta candidata B–E

Harness real opt-in por proveedor, con reporte JSON bajo reports/w2.
Todos los smokes nuevos UNTESTED. Las opciones --tools y --recovery son
adicionales y no se marcan PASS si no se ejecutan. Pi 1.0.4 permanece
incompatible; actualizar/seleccionar una instalación >=1.6,<2 explícitamente.

`--interrupt` opt-in: Codex, Pi y Gentle Shell según capacidades; OpenCode
UNSUPPORTED. El smoke no cambia permisos ni simula una cancelación exitosa.

### Primer smoke completo — 11a943d

| Proveedor | Chat | Herramienta | Recovery | Interrupt | Stop |
|---|---|---|---|---|---|
| OpenCode 2.4.2 Attached | PASS | PASS | PASS | UNSUPPORTED | PASS |
| Codex 0.172.0 | PASS | FAIL | UNTESTED | UNTESTED | FAIL |
| Pi 1.6.1 | FAIL | UNTESTED | UNTESTED | UNTESTED | FAIL |
| Gentle Shell 4.8.0 | PASS | PASS | PASS | PASS | FAIL |

Descubrimiento y apertura PASS en los cuatro. Correcciones posteriores
candidatas UNTESTED. No asumir que el PASS de Attached acredita Owned.

Stop-only en 9932566: OpenCode Attached y Codex Owned PASS; Pi stop FAIL
(exit no cero); Gentle Shell discovery BLOCKED (versión desconocida).
Corrección de cierre JSONL y diagnóstico de discovery candidatos UNTESTED.

Gentle Shell bundle observado: current `5.0.0-1.6.1`, Node `24.21.0`.
El marcador de instalación no sustituye `--version`. Resolver bundle
nativamente; 5.0.0 candidato experimental exacto con opt-in, no soportado
para release hasta conformance. El informe a8b8413 solo contiene Gentle Shell;
resultado nuevo de Pi no aportado.
