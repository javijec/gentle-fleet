# Reglas para agentes de desarrollo — Gentle Fleet 2.0

## Contexto

Este repositorio es un reset de arquitectura. No importar código ni `Cargo.lock` de Gentle Fleet Core/Protocol anteriores. No modificar los repositorios anteriores. No usar Herdr como dependencia runtime.

## Flujo obligatorio por corte

1. Leer README, PRODUCT, ARCHITECTURE, API, LIFECYCLE y ROADMAP.
2. Elegir **una** historia vertical con criterio observable. Identificar qué ejecutable del agente y versión real se probarán.
3. Implementar el mínimo que atraviese API → Session Manager → Driver → proceso → eventos → API. No agregar nuevas tablas sin un caso de uso probado.
4. Escribir primero una prueba negra de proceso o contrato que fallaría sin el cambio; después pruebas unitarias de funciones críticas.
5. Validar Linux y los `cfg(windows)` afectados; no introducir dependencias Linux-only en `fleet-core` ni `fleet-contract`.
6. Documentar exactamente qué fallas/reconexiones funcionan y cuáles no, y qué integración real se ejecutó. Un test simulado no sustituye al test contra agente real.
7. Cerrar el corte si y solo si se cumplen sus criterios. Hallazgos ajenos al objetivo se registran con prioridad; no se rediseña la arquitectura por cada observación teórica.

## Reglas técnicas

- Una única autoridad para cada dato y operación; los drivers no escriben bases Fleet.
- Los eventos públicos son tipados; no enviar JSON/RPC privado del proveedor al cliente.
- Identificadores Fleet estables; identificadores del proveedor opacos y aparte.
- Nunca autoaprobar solicitudes interactivas.
- Nunca volver a enviar un prompt incierto por un timeout sin reconciliación.
- Nada de global locks de sesiones, colas ilimitadas, secretos en logs, TCP público por defecto ni suposiciones de `procfs` en componentes portables.
- La desconexión del cliente no detiene la sesión. El cierre explícito de sesión sí debe tener efecto verificable.
- El evento persiste antes de ser confirmado por la capa que lo recibe; replay detecta huecos y no inventa eventos.
- El usuario local es la frontera de confianza del MVP. No construir VFS SQLite personalizado ni servicios root para una amenaza fuera de ese modelo.
- Las dependencias están fijadas; el CI debe funcionar desde el primer hito, sin depender de repositorios privados inaccesibles.

## Reglas de entrega

Reportar: archivos, API tocada, pruebas unitarias/de proceso/real, compatibilidad Linux-Windows, limitaciones, resultados desconocidos y commits. No afirmar garantías superiores a las pruebas. No hacer merge o push a repos externos sin instrucción explícita.
