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

## Huecos a resolver mediante spikes P0

- Gentle Shell: identificar las estructuras RPC exactas, los eventos/errores, cómo pide permisos y si hay resume nativo.
- Codex: confirmar en el binario instalado versiones, métodos, notificaciones, permisos y reattach de threads.
- OpenCode: confirmar modalidad de server, API de sesiones, eventos SSE/stream y resolución de permisos en la versión elegida.
- Windows: comprobar que runner detached realmente sobrevive a cierre de daemon, bajo Job Objects/handles propios.

No completar estos huecos mediante suposiciones en el driver.
