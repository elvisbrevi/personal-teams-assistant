# Verificación de Wiki y referencias Azure/Teams

Fechas: 2026-09-30 y comprobación final el 2026-10-01. Implementación en núcleo, CLI `pta` y host compartido 0.3.0; contrato JSON 1. Se usaron `target/debug/pta` y `target/debug/personal-teams-desktop` compilados juntos con `cargo build -p personal-teams-desktop --bins`. Se cerró primero el host instalado 0.2.0. El descriptor IPC anuncia soporte Wiki; el CLI rechaza una operación/esquema Wiki contra un descriptor antiguo. No se actualizó la instalación, la GUI ni los binarios publicados.

## Recorrido real de lectura

Se auditaron las credenciales existentes antes de usarlas. El PAT de Azure y los proveedores ya configurados permitieron completar el recorrido; no se pidió, publicó ni trasladó un secreto a argumentos o URLs. Se reutilizó el checkout/catálogo privado autorizado. Los datos privados, contenido, identidades y URLs de la cuenta quedan fuera de ejemplos y fixtures públicos.

| Operación | Resultado observado |
| --- | --- |
| `azure wiki list` | 82 wikis accesibles, incluyendo Wiki de proyecto y de código, con repositorio, carpeta y versiones publicadas. Funciona también para una fuente registrada deshabilitada. |
| `azure wiki search` | Búsqueda nativa por el procedimiento de despliegue: cuatro páginas seleccionadas, con contribución propia y último editor de terceros verificados. Declaró cobertura parcial por rutas Search sin resolución y el límite de páginas. |
| `azure wiki read` | Markdown actual de una página accesible; ruta canónica, URL Wiki, versión publicada, ETag y commit Git coherentes. Contribución propia clasificada `edited_by_me`; lectura sin cobertura parcial. |
| `test simulate` | Respuesta de 1810 caracteres respaldada por esa página, una fuente usada y su enlace verificado. Control final: confianza 0.83, motivo `supported_answer`. El estado lógico `sent` corresponde exclusivamente al adaptador de simulación (`simulation-only`); no hubo envío a Graph/Teams. |

La última compilación repitió la simulación el 2026-10-01: `supported_answer`, 1956 caracteres, una referencia/ID utilizado, enlace Wiki y `partial=false`. También se repitieron descubrimiento (82 wikis), búsqueda acotada (una página) y lectura: contenido/revisión/contribución verificados, sin cobertura parcial. La ayuda y la skill incorporada anunciaron 0.3.0 y su referencia de operación Wiki.

La prueba usó una fuente Wiki independiente: alta deshabilitada, audiencias vacías y procesamiento externo inicialmente desautorizado. Se habilitó y se autorizó procesamiento externo únicamente para su simulación local, manteniendo vacías las listas de conversaciones/remitentes. La fuente acotada a una Wiki para la verificación se retiró al terminar. No se copiaron permisos de actividad ni se amplió el catálogo. El servicio de escucha/envío de Teams permaneció detenido.

La API real confirmó que Search devuelve rutas Git, distintas de las rutas Wiki. La implementación resuelve los metadatos de páginas en lugar de convertir guiones/espacios/extensiones a mano. También confirmó que ETag puede identificar un blob: Git Items comprueba ese contenido y obtiene el commit antes de consultar autoría. Un cambio concurrente conserva la página como evidencia con autoría desconocida y cobertura incompleta, sin reutilizar atribución obsoleta.

## Regresiones reproducibles

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
```

70 pruebas pasan (37 del núcleo, 25 de integración, 8 del host/CLI). Cubren Wiki de proyecto/código, carpeta y ramas publicadas, Unicode/espacios/porcentajes, enlaces de versión, ámbito y deduplicación; creación/edición/importación y autores desconocidos; paginación, indexación, límites, permisos, timeout, documentos grandes y revisiones cambiantes; routing Wiki antes de actividad; schema/stdin/compatibilidad; referencias inventadas/ausentes/fuera de ámbito, presupuesto de caracteres, work items padre, ejecuciones/definiciones/stages homónimos; Teams con dos interlocutores, intervención propia, nombres redactados y rechazo de atribuciones intercambiadas. Las consultas locales no invocan LLM ni Graph y las simulaciones usan un adaptador sin envío.

El ejemplo `cargo run --example wiki_gate_smoke`, con la credencial existente de Jev cargada por el operador, ejecutó además tres controles finales reales usando solamente hechos sintéticos:

| Caso | Resultado |
| --- | --- |
| Procedimiento genérico con Wiki citada | Aprobado, confianza 0.80. |
| Stage concreto sin referencia verificable | Rechazado, confianza 0.45. |
| Interlocutores Teams intercambiados | Rechazado, confianza 0.58. |

Los umbrales y los controles de privacidad/promesas no se relajaron. La respuesta se valida completa, con citas y atribución ya incorporadas. Las simulaciones reales que introdujeron un stage sin vínculo se rechazaron y no enviaron mensajes.

El 2026-10-01, al operar el servicio para una prueba de chat propio, se encontró además un fallo de cancelación del arranque: si desaparecía el emisor del canal de parada sin publicar `true`, el servidor cerraba pero sus tareas podían seguir reintentando consultas y conservar el bloqueo del directorio de estado. Se añadió una regresión que falla con ese comportamiento y pasa al tratar el cierre del canal como petición de parada en todas las tareas. La suite completa pasa ahora 71 pruebas; Clippy y la compilación CLI/host también pasan. Esta regresión no sustituye la comprobación de recepción/envío real en Teams.

## Corrección de selección y operación del 2026-10-01

Una pregunta real de chat propio recibió una aclaración basada en la documentación operativa: la auditoría confirmó `tools=[]`, por lo que no se había consultado Wiki. La [evaluación de Jev](jev-evaluation.md) reproduce el veto previo y registra su sustitución por clasificación de intención, recuperación autorizada y selección tipada de referencias después de redactar. El cambio sirve para consultas documentales generales, sin una regla especial para el nombre del microservicio.

La suite final pasa 79 pruebas (41 del núcleo, 30 de integración y ocho del host/CLI), además de Clippy con advertencias como errores, formato y `git diff --check`. Las nuevas regresiones cubren intención sin catálogo, preguntas documentales implícitas, contexto previo que no altera la consulta actual, audiencias de Teams, selección de herramientas con esquema cerrado, selección tipada de referencias frente a sugerencias malformadas, rechazo de contratos inválidos y respaldo semántico/atribución de Teams. La ejecución real de `wiki_gate_smoke` pasó sus 12 casos sintéticos: una selección tipada, ocho controles finales y tres intenciones.

Se recompilaron juntos CLI y host 0.3.0 y se reinició el servicio con esos ejecutables. El recorrido final desde el CLI observó:

| Operación | Resultado con la compilación final |
| --- | --- |
| `azure wiki list` | 82 wikis, sin cobertura parcial. |
| `azure wiki search` | Cuatro páginas para el identificador compacto solicitado; encontró la página propia con título espaciado. Cobertura parcial declarada. |
| `azure wiki read` | Página propia `edited_by_me`, contenido, enlace, ETag y commit Git verificados; mismo ID compacto que Search y `partial=false`. |
| `test simulate`: uso de microservicio | `supported_answer`, confianza 0.66; 3358 caracteres dentro del límite detallado 8000; tres páginas usadas, enlaces presentes y atribución verificada de terceros. |
| `test simulate`: notificaciones de pago | `supported_answer`, confianza 0.72; 1205 caracteres dentro del límite normal 3000; una página propia usada con su enlace. |

Ambas simulaciones registran `search_azure_devops_wiki`, cobertura parcial explícita y todos los IDs usados dentro del registro verificado. El estado `sent` es exclusivamente local, del adaptador `simulation-only`. No se enviaron mensajes de prueba a Graph/Teams ni se modificó una Wiki.

La autorización posterior del usuario habilitó las tres fuentes existentes para el chat propio, conversaciones directas y menciones en grupos. Este ajuste corrige el estado inicial de audiencias vacías descrito arriba: fue una operación separada solicitada expresamente. La corrección de Jev conservó íntegra la configuración de esas fuentes antes/después, sin copiar permisos globales ni ampliar el catálogo. La skill de operación del repositorio, su versión incorporada y la copia local del agente incluyen el comportamiento nuevo.

Tras el reinicio, `status` confirmó host 0.3.0, `running=true`, túnel activo y una suscripción Graph activa; `self-chat status` confirmó recepción por webhook/polling acotado, sin salidas pendientes. El servicio local `/healthz` respondió 200 y el endpoint público devolvió 200 y el token de validación sintético exacto. El servicio queda activo para una nueva pregunta del usuario. La recepción/envío anteriores en chat propio constan en auditoría, pero todavía no hay una nueva pregunta de Teams procesada con esta compilación: las simulaciones no sustituyen esa observación. La modalidad de escritorio admite chats directos y grupos con mención; no se suscribe a publicaciones de canales de un equipo, como General.

## Límite pendiente y operación

No se modificó una Wiki real para probar invalidación. La regresión controlada devuelve contenido y revisión distintos en dos lecturas, vuelve a comprobar acceso y evita conservar autoría anterior. Para completar la observación externa, el operador debe editar de forma inocua una página autorizada y repetir `pta --json azure wiki read SOURCE_ID` con el mismo `wiki_id/path`: comprobar el nuevo contenido/ETag y atribución coherente. Se solicitó esa edición opcional; no se recibió confirmación ni se afirma que ocurrió.

Ayuda y administración: [CLI](cli.md#azure-devops-wiki-núcleo-y-host-030), [skill de operación](../desktop/src-tauri/skills/personal-teams-assistant/references/azure-wiki.md). GUI y distribución siguen pendientes; combinar actividad y Wiki mediante varias herramientas por respuesta es evolución posterior en [el roadmap](roadmap.md).
