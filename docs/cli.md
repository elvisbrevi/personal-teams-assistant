# GUI, CLI y agente

La instalación principal es mediante Cargo desde crates.io. El paquete `personal-teams-desktop` 0.2.0 entrega la GUI `personal-teams-desktop`, el CLI `pta` y la skill de operación incorporada. Requiere Rust y los prerrequisitos nativos de Tauri para compilar:

```sh
cargo install personal-teams-desktop --version 0.2.0 --locked
personal-teams-desktop
pta --version
```

El directorio `bin` de Cargo (normalmente `~/.cargo/bin`) debe estar en `PATH`. Ambos ejecutables comparten perfil, credenciales y servicio. `pta start` inicia el asistente con la GUI oculta; `pta app open` muestra su ventana. El CLI antiguo del núcleo sigue disponible por separado con `serve`, `check` y `local-info`; usa sus reglas de entorno y OAuth confidencial existentes.

Los binarios precompilados y bundles son alternativas accesorias. Para la alternativa macOS, arrastra la app del DMG a Applications. Sus dos ejecutables están en Contents/MacOS. Si elegiste ese bundle, habilita el CLI en un directorio elegido que ya esté en PATH:

```sh
"/Applications/Personal Teams Assistant.app/Contents/MacOS/pta" app install-cli "$HOME/.local/bin"
pta --version
```

El enlace apunta al ejecutable de la app y se actualiza junto con ella. El comando rechaza destinos ocupados. La implementación prevista para Windows incorpora ambos ejecutables en su directorio; PowerShell puede invocar `& 'RUTA_DE_INSTALACIÓN\pta.exe' --help`. `app install-cli DIRECTORIO` permite copiar la consola a un directorio elegido de PATH y registra la ubicación del host. Una copia de CLI en Windows requiere actualizarse junto con la instalación; no se elimina automáticamente al desinstalar la app. Los datos y credenciales no se borran por quitar los ejecutables.

`pta skill show`, `pta skill path` y `pta skill install DIRECTORIO_ABSOLUTO_NUEVO` entregan la skill de esa versión. Una instalación explícita rechaza archivos/directorios existentes. La fuente canónica del crate es `skills/personal-teams-assistant`; Cargo y bundles incorporan sus mismos textos compilados.

## Proceso y datos

```mermaid
flowchart LR
  GUI[Ventana o bandeja] --> D[Despachador serializado]
  CLI[pta] --> IPC[HTTP loopback con token de instancia]
  IPC --> D
  D --> HOST[Host de escritorio, visible u oculto]
  HOST --> PROFILE[Config, mapa y almacén de credenciales del sistema]
  HOST --> SERVICE[Un servicio, SQLite y un túnel propio]
  SERVICE --> WEB[Listener público limitado a Graph y healthz]
```

La CLI inicia el mismo ejecutable con `--host` cuando necesita un dueño. `stop` detiene servicio/túnel; `app quit` también termina el host. Cerrar la ventana conserva el host. El lock del perfil se adquiere antes de inicializar archivos; el lock de datos excluye el servicio antiguo y los probes Microsoft. Nunca se envían señales basadas solo en un PID.

En macOS/Windows se usan las mismas rutas de Tauri (`dev.personalteams.assistant`) y el perfil `default` del almacén del sistema. El directorio de datos importado se conserva. La clave de cifrado existente debe seguir validando los tokens; una clave diferente se rechaza. El primer acceso o un cambio de firma puede requerir autorización protegida del sistema.

El endpoint administrativo usa un puerto loopback separado y token aleatorio por instancia, guardado en un archivo privado. Rechaza Origin, autorización ausente o de otra instancia; no expone CORS ni rutas administrativas en el listener Graph. Los archivos usan permisos 0600/0700 en Unix y ACL de la cuenta en Windows.

## Contrato 1

`--json` devuelve un documento con contract, version, ok, code, exit_code, message, data y revision. La ayuda y versión también admiten JSON. No se imprimen credenciales ni cuerpos de errores de proveedores. Progreso/diagnóstico va a stderr. No hay streaming en este contrato.

| Exit | Significado |
| --- | --- |
| 0 | Operación concluida |
| 1 | Operación fallida; consultar estado antes de repetir |
| 2 | Entrada o validación inválida |
| 3 | Configuración/conexión pendiente |
| 4 | Autorización pendiente de persona |
| 5 | Dependencia, red o instalación no disponibles |
| 6 | Conflicto de revisión/contrato/estado |

`--non-interactive` inicia OAuth devolviendo URL/código sin abrir navegador; nunca acepta consentimiento. `auth PROVIDER finish --wait` tiene un máximo de 600 segundos; sin wait devuelve el estado inmediato. `cancel` termina la espera. Login Microsoft pausa el servicio; cancelación/fallo puede dejarlo detenido y requiere inspección. Logout elimina tokens locales; no revoca el consentimiento remoto.

Las mutaciones pasan por el mismo dueño y las operaciones basadas en un snapshot verifican revisión. Guardado valida antes de parar, prepara un journal de recuperación, escribe archivos privados y reinicia si estaba ejecutándose. Un reinicio fallido no equivale a servicio activo: consulta status para conocer configuración aplicada y estado cargado. Las operaciones de red son acotadas; tras timeout no repitas una mutación antes de inspeccionar. Auditoría usa conexiones de lectura y no recupera jobs.

Consulta `pta --help` para la superficie completa y [el inventario de paridad](cli-parity.md) para su relación con operaciones y verificaciones. `config show/get/set/apply/validate` expone todos los campos serializados del esquema. `config apply/validate` lee un objeto JSON/TOML con config y map (tunnel_config es opcional). Reemplazar objetos/listas requiere conservar los elementos deseados. Cambiar data_dir requiere importación explícita con servicio detenido. Quitar repositorios preserva archivos y rechaza mapas con fuentes dependientes inválidas.

## Respuestas y chat personal

`policy.max_answer_chars` limita una respuesta normal (1–4000); `policy.max_detailed_answer_chars` limita una detallada (hasta 16000 y nunca menor que la normal). En perfiles anteriores el segundo campo toma 8000 por defecto. DeepSeek decide el modo según solicitud y contexto, devuelve JSON tipado y recibe ambos límites. Su API configura tokens, mientras la aplicación verifica caracteres Unicode antes del control final y el envío. Una respuesta que excede el límite queda sin enviar; no se corta una afirmación aprobada a la mitad. Los saludos usan el límite normal. La API Graph tiene además límites de tamaño del cuerpo.

`self-chat enable [ID]` comprueba /me, chat oneOnOne y miembros exclusivamente del usuario conectado. El hilo especial de notas `48:notes` no expone metadata/membresía ChatThread: se valida su colección delegada `/me/chats/48:notes/messages` y que todos sus mensajes de usuario tengan el autor de la cuenta conectada. Debe existir al menos una nota para comprobarlo. Es un candidato que se verifica, no una regla universal para toda cuenta. Ver [notas personales de Teams](https://learn.microsoft.com/en-gb/answers/questions/908538/chat-id-returns-unknown-error) y [lectura delegada de mensajes](https://learn.microsoft.com/en-us/graph/api/chat-list-messages). Habilitación persiste un instante de corte; solo preguntas creadas después se procesan. Las fuentes conservan sus audiencias y deben autorizarse al chat exacto. La recepción combina webhook con polling cada 10 segundos de una página de hasta 50 mensajes, ordenada por creación; mantiene cursor de diagnóstico y deduplicación en SQLite. Gaps mayores quedan para atención manual, sin reproducir historial antiguo.

Cada salida personal tiene ID y un enlace PTA con nonce registrado antes de enviar. Esto permite reconocer webhooks adelantados y envíos ambiguos incluso tras reiniciar. Un mensaje humano que repite el texto de una respuesta sigue siendo elegible. Si Graph elimina la marca y además se pierde el resultado del envío, el chat personal se pausa mientras exista una salida sin ID, en vez de arriesgar un bucle. `self-chat status` informa las salidas sin resolver; requiere reconciliación manual. Un envío nunca se reintenta automáticamente. Los mensajes propios en otros chats siguen excluidos.

`doctor --offline` valida archivos; doctor inspecciona metadatos y distingue salud persistida/cargada. `test providers` usa datos ficticios y consume API; `test connectivity` lee la cuenta Graph. `test simulate` y chat usan el pipeline local y nunca envían a Graph. `test self-chat` comprueba membresía y declara que no verificó extremo a extremo. El cierre real exige una pregunta nueva, auditoría de envío y respuesta visible en Teams, con prueba de reinicio y ausencia de bucles.

## Roadmap Windows (próxima versión)

Windows queda fuera de esta entrega por decisión del usuario. Se conserva su compilación existente en CI; quedan para la próxima versión el instalador conjunto, actualización de CLI/PATH, pruebas funcionales de consola/bandeja/Credential Manager/ACL, inicio-parada-reinicio, desinstalación sin pérdida de datos y firma según los medios disponibles. El código común conserva los puntos de integración, sin declarar validación funcional ni distribución estable Windows.

Ante una salida ambigua sin marca recuperable, `self-chat status` lista pending_outputs con nonce y fecha. Tras identificar personalmente su mensaje en Teams/Graph, `pta self-chat reconcile NONCE MESSAGE_ID` registra la correspondencia explícita; valida cuenta, conversación y fecha y no reenvía nada. El agente necesita que la persona identifique esa salida si Graph no conservó la marca.

## Azure DevOps Wiki (núcleo y host 0.3.0)

La compilación del repositorio incorpora Wiki; la distribución 0.2.0 indicada arriba todavía no la incorpora. Compila y usa ambos ejecutables juntos:

```sh
cargo build -p personal-teams-desktop --bins
./target/debug/pta --version
./target/debug/pta --json capabilities
```

Cierra antes un host antiguo con su propio `pta app quit`. El descriptor del host anuncia soporte Wiki: el CLI nuevo rechaza operaciones/esquemas Wiki si el host no lo anuncia, con exit 6. No reinstales ni publiques para probar esta compilación.

Registra un recurso `kind=tool`, inicialmente `enabled=false`, `external_processing=false`, `allowed_conversations=[]` y `allowed_senders=[]`, mediante `sources add`. Usa un repositorio/catálogo existente y su `secret_ref` (ver [mapa de ejemplo](../knowledge-map.example.toml)). El catálogo conserva `organization`, `projects` y `author_email`; no tiene nuevos campos. `wiki_ids=[]` admite las wikis de esos proyectos; una lista de UUID restringe más. `author_mode` admite `prefer_mine`, `mine_only` y `all`; `mine_only` acepta creación **o edición** propia verificadas por página/revisión.

```sh
pta --json azure wiki list SOURCE_ID
pta --json sources enable SOURCE_ID
pta --json azure wiki search SOURCE_ID <<'JSON'
{"query":"despliegue continuo","wiki_id":null,"author_mode":"prefer_mine"}
JSON
pta --json azure wiki read SOURCE_ID <<'JSON'
{"wiki_id":"11111111-abcd-abcd-abcd-111111111111","path":"/Procedimiento de despliegue"}
JSON
```

`list` funciona con una fuente deshabilitada. `search/read` requieren habilitarla y seleccionarla explícitamente. Reciben JSON cerrado por stdin: no aceptan organizaciones ni URLs arbitrarias. La ruta de lectura es la **ruta canónica** de Wiki devuelta por Search/Pages, no la ruta Git `.md`. Cero coincidencias es éxito; consulta inválida usa exit 2, fuente deshabilitada exit 3 y permisos/red/API exit 5. Un 403 explica `vso.wiki`; la atribución Git requiere `vso.code`. Los cuerpos privados de errores y los correos no se imprimen.

La salida conserva el envelope de contrato 1 y añade en `data`: `query`, `wikis`, `pages`, `candidates`, `histories_checked`, `partial` y `warnings`. Cada página incluye wiki/proyecto/repositorio/carpeta/versión, título, rutas Wiki/Git, contenido redactado y `reference` (ID, URL verificada, autoridad, nombre y rol cuando estén disponibles). `revision` es el ETag del contenido; `git_revision` y la revisión de la referencia son el commit del repositorio comprobado contra ese contenido mediante Git Items. Una modificación concurrente impide reutilizar la atribución anterior. No hay caché de Markdown ni de autoría.

Search pagina de 25 en 25, con hasta 100 candidatos globales; resuelve hasta 10 candidatos/historias y entrega hasta cuatro páginas para contexto. Las lecturas son secuenciales (concurrencia ≤4), con 1 MB por respuesta y plazo Wiki global de 30 segundos. La simulación local tiene 110 segundos, por debajo del IPC de 120. Indexación pendiente, recortes, páginas ilegibles y errores Git se explican con `partial/warnings`; no equivalen a ausencia de información. Un fallo Git permite contenido con autoría desconocida; un fallo Pages excluye la página.

Estos comandos locales no invocan modelos ni Graph, y no conceden audiencias. Para `chat/test simulate` autoriza `external_processing` **en esa fuente** mediante `sources audience`, conservando las listas de audiencias vacías si el uso es solo local. Una pregunta explícita sobre Wiki tiene prioridad incluso si contiene «pipeline». Conviene formular el tema en la primera frase; no se envía historial ni instrucciones posteriores como búsqueda. Varias fuentes Wiki usan el enrutamiento autorizado existente.

Las simulaciones añaden `used_sources`, `references`, `partial` y `warnings` a `data`; las respuestas conservan también remitentes Teams en la auditoría. El pipeline añade las citas antes de validar privacidad, longitud y Jev: toda página utilizada y todo work item/pipeline/stage concreto mencionado necesita referencia verificable. Las etapas sin enlace propio usan la ejecución o configuración contenedora con etiqueta precisa. La documentación propia permite autoridad documental, sin probar ejecución; terceros llevan ubicación y autor/último editor, o se declara autor desconocido. Teams conserva autor, fecha, mensaje e intervención, sin inferir interacción por pertenencia al grupo. `audit show --content` permite inspeccionar texto; sin él se omiten respuestas y texto de mensajes Teams.

Ver [verificación Wiki](azure-devops-wiki-verification.md) y [roadmap](roadmap.md) para evidencia real y diferencias de GUI/distribución.
