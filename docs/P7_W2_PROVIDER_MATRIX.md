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
