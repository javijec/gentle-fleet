# P7-W2 — instalaciones observadas en Windows

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
