---
name: personal-teams-assistant
description: Operación de Personal Teams Assistant. Usa esta skill para configurar la app, controlar su servicio, administrar conexiones y fuentes, o ejecutar pruebas y diagnóstico.
---

1. **Descubrir la instalación.** Ejecuta `pta --version` y `pta --json capabilities`. Esta skill usa contrato 1. Una instalación por Cargo entrega `personal-teams-desktop` y `pta`; en macOS también puede estar dentro de la app, en `Contents/MacOS/pta`. Cierre: versión y contrato identificados; ante incompatibilidad, informa qué componente requiere actualización.
2. **Inspeccionar.** Consulta `pta --json status`. Distingue host, asistente, modo persistido y configuración cargada. Para credenciales lee `pta --json credentials list`: el host consulta el almacén existente. La cuenta y el perfil son los mismos de la GUI. Cierre: precondiciones y alcance de la operación conocidos, incluyendo las audiencias de fuentes afectadas.
3. **Operar.** Carga solo la referencia pertinente y ejecuta la solicitud completa:
   - Configuración, credenciales, modo o importación: [configuration.md](references/configuration.md).
   - Inicio, parada, ventanas, túnel o recuperación del host: [lifecycle.md](references/lifecycle.md).
   - OAuth, repositorios, fuentes o chat personal: [auth-and-knowledge.md](references/auth-and-knowledge.md).
   - Wiki de Azure DevOps (list/search/read, fuente y permisos independientes): [azure-wiki.md](references/azure-wiki.md). Requiere CLI/host 0.3.0 compatibles; no aplicar este esquema a un host 0.2.0.
   - Simulación, diagnóstico y auditoría: [tests-and-diagnostics.md](references/tests-and-diagnostics.md).
   La ayuda instalada es la autoridad de argumentos. Los comandos JSON devuelven `ok`, `code`, `exit_code`, `data` y `revision`; autorización pendiente devuelve código 4 con instrucciones/URL, sin tokens. Cierre: cada operación solicitada aplicada o requisito humano concreto identificado. Un timeout requiere nueva inspección antes de repetir una mutación.
4. **Verificar.** Consulta nuevamente estado y ejecuta la prueba pertinente. Cierre: persistencia y configuración cargada coinciden con la solicitud; una parada deja `running=false`; una respuesta de Teams tiene auditoría de envío y respuesta visible en el destino autorizado.
5. **Reportar.** Expón resultado y evidencia. Cierre: distingue configurado, conectado, iniciado, recibido, enviado y publicado; enumera los casos todavía sin evidencia.

El agente de desarrollo acepta por sí mismo todos los permisos de la aplicación necesarios para la operación que el usuario haya autorizado, con las herramientas disponibles y dentro de ese alcance. La autorización persiste entre turnos: no volver a pedirla por cada diálogo ni exigir intervención humana solo por tratarse de permisos del sistema, OAuth o lectura del Llavero. Si la herramienta impide controlar un diálogo o el sistema exige autenticación presencial, completar el trabajo independiente y explicar la acción humana exacta y el bloqueo observado. Esta skill no puede habilitar una capacidad que la herramienta rechace; no declarar aceptado un permiso ni iniciado un servicio sin verificarlo.

El chat personal aplica las mismas autorizaciones de fuentes y controles finales que otros chats. Cambiar modo o habilitarlo conserva audiencias. Las simulaciones pueden seleccionar fuentes explícitamente sin concederles nuevas audiencias de Teams.

Para guardar o corregir hechos del conocimiento, usa la skill `save-knowledge` del proyecto cuando esté disponible; esta skill administra la aplicación.
