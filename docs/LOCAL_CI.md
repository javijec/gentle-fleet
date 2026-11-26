# CI local reproducible (Windows + Linux)

No requiere GitHub Actions ni servicios de pago. Requiere Python 3.10+, Git,
Rust estable con rustfmt/clippy y dependencias nativas del workflow existente.
Windows: PowerShell 7, MSVC y Windows SDK. Linux: Bash y OpenSSH server (sshd). El workspace headless no requiere GPUI ni bibliotecas gráficas.
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
del workspace headless (incluido control de `cargo metadata --locked`), regresiones nativas Windows y todos los E2E
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

## Pruebas interactivas en ambos equipos

Desde Windows: `python scripts/ci/both.py`. Desde Linux:
`python3 scripts/ci/both.py`.

El coordinador pide modo (quick/full/w1c; w1c por defecto), alias SSH de
Windows y Linux (usar `local` para el equipo donde se ejecuta) y la ruta
absoluta del repositorio para cada equipo remoto. También acepta ambos
equipos por SSH. Ejemplo no interactivo desde Windows:

```powershell
python scripts/ci/both.py --mode w1c --windows local --linux linux --linux-root /home/USUARIO/dev/gentle-fleet
```

Preparar previamente los checkouts limpios en el mismo SHA que el
coordinador; el script verifica plataforma y SHA, no hace pull ni cambia
ramas. Se requiere OpenSSH con autenticación no interactiva y claves de host
conocidas; en el shell remoto debe estar disponible python (Windows) o
python3 (Linux), y las herramientas de CI nativas. Las rutas remotas admiten
espacios. Se usa el CI existente en cada plataforma, sin WSL para Windows.
Modo w1c añade diez ejecuciones de fleet-protocol en Windows si full pasa.
Se intenta cada plataforma aunque la otra falle; no se declara PASS si
falta un reporte, el SHA no coincide, hay un SKIP/BLOCKED o falla un check.
Los informes JSON se reúnen en reports/combined/<id>/combined.json y los
logs quedan locales a cada host. AUTOMATED_PASS no sustituye revisión
manual del gate W1-B/W1-C ni evidencia de release. No guarda alias,
contraseñas ni claves en una configuración nueva.

## Un comando por SSH (opcional; interfaz anterior)

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
al mismo SHA y plataforma, para proveedores reales, SSH físico, smoke de consumidor externo SDK/CLI,
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
Para Linux usar python3 y evidencia Linux. La evidencia `external-sdk-smoke` debe demostrar catálogo, sesión, prompt, timeline, reconexión/cancelación y parada de un consumidor externo headless mediante el SDK/CLI; no se sustituye por una captura ni por una ventana. El perfil release no acredita macOS,
instaladores, ni requisitos de release adicionales del roadmap. Los cierres
siguen sujetos a QUALITY.md y a los criterios de la etapa. GitHub Actions
permanece intacto y su estado BLOCKED se registra por separado.

## Verificación del propio CI local

python (Windows) o python3 (Linux) scripts/ci/test_local_ci.py comprueba
perfiles, estados, rechazo de reportes incompletos y evidencia de release.
También se ejecuta en quick/full/release. No constituye una prueba nativa
de las named pipes; ejecutar full en las dos plataformas para ello.

## Salida de consola

El CI presenta RUN en cian, PASS en verde, FAIL en rojo y BLOCKED/SKIP en amarillo en terminales ANSI compatibles. El coordinador conserva colores en la salida SSH sin colorear JSON. Cada check muestra su duración; las rutas de logs y reportes se imprimen solo cuando el resultado no es PASS. Los logs e informes completos siguen guardándose incluso cuando todo pasa. Usar NO_COLOR para salida plana. No se modifican los criterios de aceptación ni la ejecución de pruebas.

## Formato previo y sincronización opt-in

Desde una rama local limpia y publicada en origin, ejecutar:

```powershell
python scripts/ci/both.py --mode w1c --windows local --linux javiomarchi --linux-root /home/javijec/dev/gentle-fleet --auto-format
```

Antes del gate, verifica que HEAD coincide con la rama origin, ejecuta cargo fmt --all y, si hay cambios exclusivamente rastreados .rs, muestra el diff y pide confirmación para commit/push. Si se rechaza, deja los cambios para revisión y no ejecuta CI. Si no hay cambios, no crea commit. Luego sincroniza el checkout SSH limpio al SHA exacto publicado mediante fetch de la rama explícita y checkout detached sin sobrescribir archivos ignorados. Rechaza HEAD remoto divergente, cambios locales, fallos SSH y push rechazado; no usa force, reset, clean ni stash. No modifica ramas locales remotas, incluida main. Mantiene cargo fmt --check obligatorio en ambos equipos y el gate mismo SHA. Sin --auto-format conserva el modo anterior sin modificar Git. Primero actualizar el coordinador local con git pull --ff-only. La sincronización no instala herramientas ni modifica host keys.


## Gate de aceptación W1-F

`both.py --mode w1f` ejecuta full Windows/Linux sobre el mismo SHA limpio y exige protocolo Windows 10/10 más Lifecycle 20/20 (`-Scenario Lifecycle`). Los registros `protocol_runs` y `lifecycle_runs` quedan en windows.json; combined.json cuenta ambos grupos además de los checks full. `w1f-acceptance` forma parte de full Windows y verifica concurrencia, aprobaciones y backpressure/replay mediante API pública. Se conserva el modo w1c y su validación anterior. AUTOMATED_PASS no certifica el gate manual ni release. Ver P7_W1_F_PLAN.md.
