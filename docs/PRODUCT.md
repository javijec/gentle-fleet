# Producto y límites de Gentle Fleet v2

## Usuario y problema

Una persona trabaja con varios proyectos en Linux y Windows, con agentes instalados y autenticados en cada máquina. Quiere administrar sesiones desde CLI, clientes externos y máquinas remotas, sin depender de la ventana que inició el proceso. Necesita ver conversaciones, herramientas, permisos y estado coherentes sin abrir un PTY como sustituto de chat.

**Alcance headless:** Fleet distribuye daemon, runner, CLI, SDK y protocolo; no desarrolla una interfaz gráfica propia. Gentle Shell GPUI es un consumidor externo opcional.

## Principios de producto

- **Chat primero.** El flujo principal es texto, turnos, herramientas y solicitudes interactivas; terminales y archivos son complementos.
- **Un mismo contrato para todas las interfaces.** Toda mutación pasa por Fleet API; no existen caminos privados para clientes externos.
- **Una sesión pertenece a un host y un workspace.** El host ejecuta los procesos y posee sus datos; un cliente puede mostrar varios hosts sin sincronizar sus bases.
- **Conservar identidad nativa.** Fleet Session ID identifica el control de Fleet; native session ID permite reanudar la conversación cuando el proveedor lo soporta.
- **Capacidades negociadas.** Mostrar `unsupported` cuando un proveedor no dispone de una función; no inventar soporte.
- **Desconexión ≠ parada.** Dos clientes pueden ver una sesión; uno puede desconectarse sin afectar el agente.
- **Permisos explícitos.** Nunca conceder aprobaciones por un timeout o por el simple hecho de compartir UID.
- **Simple de instalar.** Un daemon de usuario y un CLI; no root, contenedores ni Herdr obligatorios.

## Historias del MVP

1. Detectar agente disponible y versión; iniciar sesión en directorio validado.
2. Ver texto del asistente en tiempo real y los resultados de herramientas, normalizados.
3. Seleccionar permiso solicitado y recibir confirmación o resultado incierto.
4. Reconectar otro cliente usando secuencia de eventos y consultar estado real.
5. Listar sesiones y workspaces de un host; detener de forma explícita.
6. Usar la misma CLI para sesiones locales y remotas sobre SSH tras P5.

## Definiciones

- **Workspace:** directorio de ejecución concreto en un host. No equivale al repo ni a la ventana del cliente.
- **Session:** vínculo Fleet entre workspace y proveedor con identidad estable.
- **Turn:** envío del usuario y trabajo resultante; tiene ID propio, independiente del ID del mensaje.
- **Runner:** proceso auxiliar por sesión que supervisa la conexión con el proveedor y puede sobrevivir al daemon.
- **Provider session:** identidad nativa que devuelve el agente; su semántica de recuperación depende del driver.
- **Timeline:** eventos normalizados ordenados y almacenados para reconstruir UI y recuperar después de desconexiones.
- **Operation:** mutación aceptada con ID e idempotency key, con resultado durable e incertidumbre explícita.

## Métricas de éxito iniciales

- Completitud de un flujo chat/permiso con agente real.
- Cero prompts duplicados en pruebas de reintento deterministas.
- Cero eventos omitidos sin notificación de gap.
- Dos clientes simultáneos y reconexión con timeline coherente.
- Cualquier fallo del proveedor se distingue de un error de transporte.
- Las mismas pruebas de contrato se ejecutan en Linux y Windows (excepciones registradas).
