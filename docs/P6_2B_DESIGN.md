# P6.2-B — Machines y Fleet SDK en Desktop

## Alcance y arquitectura

Desktop presenta máquinas y consultas de solo lectura mediante el adapter
`apps/desktop/src/fleet/`; el adapter usa `fleet-client` y el SDK continúa siendo
el único dueño del wire y del transporte:

```text
GPUI → apps/desktop/src/fleet → fleet-client → local IPC / fleet-ssh → fleetd
```

La GUI no importa `fleet-core`, runners, drivers, persistencia ni protocolos de
agentes. La identidad es `MachineId::Local` o `MachineId::Ssh(alias)`, nunca solo
la etiqueta visible. `SessionIdentity` combina host e ID Fleet para evitar
colisiones entre máquinas.

## Catálogo y conexiones

`fleet-client::configured_hosts()` expone únicamente alias del catálogo de
`fleet-ssh::load_hosts()`, el mismo archivo que administra `fleetctl host`.
Descubrir entradas no conecta ni marca hosts como conectados. Local siempre se
presenta; los SSH requieren selección/conexión explícita. Desktop no copia
destinos, claves, credenciales ni rutas de identidad a su estado visual.
Seleccionar una máquina solo cambia la vista; conectar o actualizar requiere
acción explícita. Cambiar de selección cancela y marca como desconectada cualquier
solicitud pendiente anterior.

Los estados distinguen desconectado, conectando, conectado, reconectando, error
y no disponible. Una consulta satisfactoria es la evidencia de conexión. Los
errores se clasifican a mensajes fijos; no se representa stderr crudo ni se
incluyen rutas/destinos. La verificación OpenSSH sigue `StrictHostKeyChecking=yes`
y `known_hosts`; claves desconocidas requieren intervención externa segura y
reintento manual.

## Workers, carga y obsolescencia

`FleetClient` es síncrono: connect y las tres listas se ejecutan en el executor
de fondo GPUI, no durante render. El adapter limita a dos transportes SDK subyacentes simultáneos,
usa token cancelable comprobado entre requests y número de generación para
descartar resultados si cambió la selección. Solo el cliente seleccionado se
retiene; cambiar de host desconecta únicamente el cliente Desktop anterior, no
`fleetd`, runners ni sesiones.

La E/S síncrona que ya está dentro de una llamada SDK no puede interrumpirse a
mitad de frame. El `ConnectTimeout=10` existente limita establecimiento SSH; una
respuesta Fleet en curso puede durar hasta que transporte/proceso cierre. La
espera de GPUI tiene un deadline de 20 s, **no** un timeout de terminación del
hilo subyacente. El permiso de `FleetWorkers` se mueve al hilo real de I/O y
se libera únicamente cuando ese hilo finaliza, incluso si el usuario cancela
o GPUI deja de esperar. Por tanto no se pueden acumular más de dos conexiones
bloqueadas por reintentos. En el peor caso, ambos permisos pueden permanecer
ocupados mientras el transporte no cierre; la UI continúa respondiendo y no
admite nuevas consultas hasta que haya capacidad. La generación impide que
un resultado atrasado sobrescriba la selección nueva. No se añade Tokio ni una
cola ilimitada.

## Consultas y límite con P6.2-C

Al conectar se consultan `list_agents`, `list_workspaces` y `list_sessions`
reales. La vista muestra agentes disponibles/no disponibles, workspaces y
sesiones tipadas con estado/proveedor/workspace. Abrir una sesión solo muestra
datos públicos ya consultados. Chat, timeline, mutaciones y creación de sesión
quedan fuera de B; la acción de chat se presenta deshabilitada.

## Ciclo de vida

Abrir Desktop y leer catálogo no inician agentes ni conexiones remotas.
Desconectar/cerrar el cliente cierra el transporte del SDK únicamente. No se
envían `StopSession` ni operaciones de control al daemon. Un `fleetd` local
ausente se muestra como no disponible; Desktop no lo arranca para ocultar el
error.
