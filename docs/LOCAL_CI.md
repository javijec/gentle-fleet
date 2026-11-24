# CI local reproducible (Windows + Linux)

No requiere GitHub Actions ni servicios de pago. Requiere Python 3.10+, Git,
Rust estable con rustfmt/clippy y dependencias nativas del workflow existente.
Windows: PowerShell 7, MSVC y Windows SDK. Linux: Bash, OpenSSH server (sshd),
dependencias de GPUI X11/Wayland/Vulkan y entorno que satisfaga el fixture GPUI.
Ejecutar como usuario normal, no root. En Linux, si sshd no está en PATH,
añadir su directorio (por ejemplo /usr/sbin). No desactivar host-key checking.

## Uso desde la raíz de un checkout limpio

Windows:
```powershell
pwsh -NoProfile -File scripts/ci/windows.ps1 -Profile quick
pwsh -NoProfile -File scripts/ci/windows.ps1 -Profile full
```
Linux:
```bash
bash scripts/ci/linux.sh quick
bash scripts/ci/linux.sh full
```

También disponibles quick.ps1 y quick.sh. Full ejecuta los comandos Rust
del workspace (incluido GPUI), regresiones nativas Windows y todos los E2E
deterministas Linux del workflow. Los jobs duplicados se consolidan; no replica
la imagen limpia Ubuntu/Windows de GitHub ni instala dependencias por su cuenta.
No usa credenciales de proveedores ni activa GF_E2E_REAL. Tests de fixture
pueden ejecutar procesos locales. Un build fallido bloquea sus E2E dependientes;
los pasos independientes siguen. SKIP/BLOCKED impiden PASS global.

## Reportes y validación conjunta

reports/<plataforma>-<perfil>.json y reports/logs/ contienen commit, plataforma,
arquitectura, versiones, comandos, estados, duración y logs locales. reports/
está ignorado por Git: no subir automáticamente logs, rutas, tokens ni datos
personales. Revisar y redactar cualquier evidencia compartida. En Windows,
guardar el checkout en un directorio privado del usuario. Un checkout con
cambios rastreados o no ignorados no se acepta como evidencia de un commit.
Si se interrumpe una corrida, queda BLOCKED; no reutilizar reportes antiguos.

Copiar el JSON Linux a reports/linux-full.json del checkout Windows y ejecutar:
```powershell
python scripts/ci/report.py --windows reports/windows-full.json --linux reports/linux-full.json --profile full
```
El consolidador exige SHA del HEAD, plataforma, perfil, conjunto exacto de
pruebas y resultados consistentes. No es una firma ni una atestación externa.
El resultado automático no sustituye revisión de los logs ni criterios manuales.

## Un comando por SSH (opcional)

Preparar ambos checkouts limpios en el mismo SHA y una configuración OpenSSH
ya autenticada con host key conocida. Desde Windows:
```powershell
python scripts/ci/remote.py --host linux --remote-root /home/USUARIO/dev/gentle-fleet --profile full
```
Reemplazar linux por el alias SSH real y la ruta por la de ese equipo.
Ejecuta Windows y luego Linux, recupera únicamente el JSON (no logs) a una
carpeta de corrida única y consolida. No hace pull, no instala nada, no abre
puertos, no modifica host keys. Un fallo SSH no puede convertirse en PASS.
Las rutas de log Linux son locales al host Linux. No se ejecuta release
remoto automáticamente porque requiere evidencia humana específica.

## Release (evidencia manual/real obligatoria)

El perfil release ejecuta full y exige evidencia separada del operador, vinculada
al mismo SHA y plataforma, para proveedores reales, SSH físico, smoke gráfico,
auditoría de dependencias y, en Windows, IPC/ACL entre cuentas, integridad del
runtime, backpressure/cancelación/identidad del peer y recuperación nativa.
Sin evidencia esos casos quedan BLOCKED. Las claves aceptadas están en MANUAL
de local_ci.py. Ejemplo de formato (no representa un resultado real):
```json
{
  "commit": "<SHA completo>",
  "platform": "windows",
  "checks": {
    "cross-user-ipc-acl": {
      "status": "BLOCKED",
      "evidence": "No hay segunda cuenta de prueba autorizada"
    }
  }
}
```
Ejecutar las pruebas reales opt-in siguiendo la documentación de cada etapa,
con perfiles aislados y sin registrar secretos. Revisar resultados antes de
declarar PASS. El script no inventa ni ejecuta automáticamente estas pruebas:
```powershell
python scripts/ci/local_ci.py --profile release --manual-evidence reports/windows-evidence.json
```
Para Linux usar python3 y evidencia Linux. El perfil release no acredita macOS,
instaladores, ni requisitos de release adicionales del roadmap. Los cierres
siguen sujetos a QUALITY.md y a los criterios de la etapa. GitHub Actions
permanece intacto y su estado BLOCKED se registra por separado.

## Verificación del propio CI local

python (Windows) o python3 (Linux) scripts/ci/test_local_ci.py comprueba
perfiles, estados, rechazo de reportes incompletos y evidencia de release.
También se ejecuta en quick/full/release. No constituye una prueba nativa
de las named pipes; ejecutar full en las dos plataformas para ello.
