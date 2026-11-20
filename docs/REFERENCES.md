# Referencias externas consideradas (20/11/2026)

Las siguientes fuentes informan la propuesta, **no** certifican que Gentle Fleet ya implemente sus capacidades. Verificar continuamente que las URLs y las APIs correspondan a la versión instalada.

- Paseo, arquitectura y separación daemon/protocol/client/proveedores: https://github.com/getpaseo/paseo/blob/main/docs/architecture.md
- Paseo, contrato de proveedores y adaptador ACP genérico: https://github.com/getpaseo/paseo/blob/main/public-docs/providers.md
- Orca, agentes y funcionamiento multiplataforma: https://github.com/stablyai/orca
- Orca, propiedad de estado en servidor remoto: https://github.com/stablyai/orca/blob/main/docs/site/content/docs/remote-servers.mdx
- Herdr, persistencia y acceso SSH: https://herdr.chefgroep.nl/docs/persistence-remote/
- Codex, documentación de app-server y solicitudes interactivas: https://learn.chatgpt.com/docs/app-server
- OpenCode, servidor compartido por usuario: https://opencode.ai/docs/cli
- OpenCode, API HTTP experimental: https://opencode.ai/docs/api
- OpenCode, providers y sesiones en el servidor: https://opencode.ai/docs/build/plugins/server/

## Conclusiones verificadas útiles

Paseo separa módulos de protocolo, cliente y server, y usa proveedores que reportan funciones/capacidades; incluye agentes nativos y una vía ACP genérica. Orca documenta agentes locales/remotos y un runtime en la máquina de trabajo. Herdr utiliza SSH para enlazar con el servidor persistente remoto, pero es principalmente un multiplexor terminal. Codex ofrece app-server estructurado con requests interactivas del servidor; OpenCode cuenta con arquitectura cliente-servidor y API de sesiones. **Estos modelos son inspiración, no componentes copiables ni garantía de APIs idénticas entre versiones.**

## Resultado de los spikes P0 (20/11/2026)

Los tres primeros huecos quedaron resueltos con binarios reales. El resumen está
en [`P0_FINDINGS.md`](P0_FINDINGS.md); el detalle con etiquetas de evidencia y
referencias a cada captura está en
[`research/PI.md`](research/PI.md), [`research/CODEX.md`](research/CODEX.md) y
[`research/OPENCODE.md`](research/OPENCODE.md). La comparación y las limitaciones
están en [`research/PROVIDER_COMPARISON.md`](research/PROVIDER_COMPARISON.md) y
[`research/KNOWN_LIMITATIONS.md`](research/KNOWN_LIMITATIONS.md). Las capturas
anonimizadas están en `spikes/p0/fixtures/`:

- Gentle Shell: RPC JSONL, eventos `message_update`/`tool_execution_*`, permisos
  de la extensión vía `extension_ui_request`, resume con `switch_session` y
  muerte del proceso al cerrar stdin.
- Codex: handshake `initialize`/`initialized`, `thread/*` y `turn/*`, requests
  de aprobación del servidor, `thread/resume` entre procesos y esquema
  experimental dependiente de versión.
- OpenCode: `opencode serve` con password Basic, `/api/event` SSE, sesiones y
  permisos REST, persistencia tras reinicio y servicio de usuario compartido.

Sigue abierto, y no se resolverá por suposición: comprobar en Windows que un
runner detached sobrevive al cierre del daemon (Job Objects/handles propios).
También quedan sin probar la cancelación real, la paginación avanzada y el
comportamiento ante SIGKILL del daemon; esas pruebas pertenecen a P2/P3.
