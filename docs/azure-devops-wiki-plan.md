# Wikis de Azure DevOps y referencias de actividad Azure/Teams

Estado: implementado en núcleo, CLI y host compartido 0.3.0 el 2026-09-30. Descubrimiento, búsqueda, lectura y simulación con una página real verificados sin enviar mensajes a Teams. [Informe de verificación](azure-devops-wiki-verification.md), incluidos resultados, compilación usada y limitación pendiente: no se observó una edición humana de una página real; el cambio de revisión/contenido se cubre mediante regresión controlada. GUI y distribución 0.2.0 conservan los desfases registrados en [el roadmap](roadmap.md).

## Objetivo y decisiones

El usuario confirma que su cuenta accede a las wikis y solicita que el asistente pueda buscar conocimiento allí, como consulta hoy su actividad en Azure DevOps. La entrega se concentra en el núcleo y el CLI `pta`; el desfase de GUI y binarios distribuidos se registra en [el roadmap](roadmap.md).

El usuario precisó la política de respuesta el 2026-09-30: «Si la wiki es mía editada o creada entonces puede usarla con autoridad para responder», y para documentación de otro autor pide «mencionar dónde está la info y quien la documento». También exige que todas las respuestas basadas en wikis incluyan el enlace a la fuente, o fuentes si son varias.

El usuario amplió el requisito el 2026-09-30: al mencionar work items, stages o pipelines concretos, incluir también sus enlaces. Cuando la información provenga de conversaciones de Teams, mencionar a las personas con quienes se interactuó si es relevante para la respuesta. Estos controles se aplican a las fuentes de actividad existentes además de la nueva fuente Wiki; forman parte de esta entrega del CLI.

Buscar en las wikis accesibles dentro de las organizaciones y proyectos configurados, con prioridad a las páginas creadas o editadas por la identidad indicada en `author_email`. Se permitirá restringir una fuente a IDs de wikis concretas. Evaluar la autoría por página: una contribución a una página no atribuye todas las páginas del proyecto al usuario. Crear una página, editarla y crear la wiki completa son hechos distintos.

| Evidencia de autoría de la página | Uso en la respuesta |
| --- | --- |
| Creación o edición propia verificada | Puede respaldar una respuesta directa con la autoridad del usuario. Incluir siempre el enlace de fuente. |
| Documentación de terceros | Presentar la información con atribución: autor verificado, título/proyecto/wiki y enlace de fuente. No presentarla como documentación propia. |
| Autoría desconocida o incompleta | Señalar que no se pudo verificar quién la documentó; indicar ubicación y enlace. No concederle autoridad propia ni inventar el autor. |

Usar el nombre de autor documentado, sin correo ni datos de contacto. Si solo se conoce la última edición, nombrar a esa persona como último editor registrado; no asegurar que creó la página. La contribución propia permite el uso autorizado de la documentación, pero no demuestra que el usuario ejecutó el procedimiento ni que escribió todos los cambios posteriores.

Usar la búsqueda nativa de Azure DevOps y recuperar el Markdown de las páginas seleccionadas. La primera entrega no necesita clonar wikis, generar embeddings, mantener un índice de contenido ni añadir otro proveedor de modelos. Se reutilizan `reqwest`, los límites de lectura, la resolución de secretos, el catálogo ADO y el pipeline existentes.

La búsqueda de documentación no hereda la ventana de 7/14 días de la actividad: una página antigua puede contener el procedimiento vigente. Tampoco se guarda automáticamente su contenido como un hecho permanente en la base de conocimiento; cada respuesta obtiene evidencia de la wiki.

## Puntos de integración existentes

| Archivo | Cambio previsto |
| --- | --- |
| `src/ado.rs` | Reutilizar catálogo, alcance, construcción de URLs y lectura autenticada; enriquecer la evidencia de actividad con referencias verificables a work items, pipelines y stages, preservando su relación con ejecución o definición. |
| `src/ado/wiki.rs` — nuevo módulo de `ado` | Descubrimiento, búsqueda, normalización de rutas/versiones, lectura y clasificación de autoría. Un módulo concreto sin crear otra capa de conectores. |
| `src/tools.rs` | Variante `AzureDevopsWiki`, validación, ejecución y timeout propio; conservar también metadatos de referencia en `get_work_item` y la consulta de actividad. |
| `src/adapters/graph.rs` | Conservar identidad/nombre del remitente, fecha, mensaje y relación de interacción en el contexto Teams pertinente, junto con el texto. |
| `src/pipeline.rs` | Prioridad de una petición explícita de Wiki; solicitud actual separada del contexto; referencias de páginas y entidades Azure; atribución de interlocutores Teams conservada hasta la respuesta. |
| `src/llm.rs` y `src/decision.rs` | Identificadores de fuentes/entidades usadas, instrucciones de autoridad y atribución, enlaces obligatorios y control final sobre referencias e interlocutores verificados. |
| `src/state.rs` | Reutilizar `activity_cache` solo si se necesita guardar metadatos de autoría; no añadir una base de datos de contenido. |
| `desktop/src-tauri/src/cli.rs` | Ayuda, validación de argumentos y comandos Wiki dentro del contrato JSON actual. |
| `desktop/src-tauri/src/control.rs` y `lib.rs` | Despachar las operaciones locales del CLI mediante el host compartido y resolver el perfil, fuente y credencial. Esto no requiere formularios nuevos. |
| Docs, mapa de ejemplo y skill de operación | Ejemplos sintéticos, comandos y límites; diferencias de GUI/distribución en el roadmap. |

## 1. Validar las APIs y el alcance

Primero comprobar en modo lectura el catálogo de wikis de los proyectos ya configurados. Inspeccionar los metadatos de credenciales disponibles y reutilizar el `secret_ref` de Azure DevOps antes de pedir otra credencial. En esta fase de implementación se aplica la skill `credentials`; no mostrar ni trasladar el PAT a argumentos o URLs.

Que la cuenta tenga acceso en el navegador no garantiza que el PAT configurado tenga el alcance correspondiente. Probar lectura/búsqueda Wiki con la credencial existente; ante un 403, informar el permiso específico que falta. La API Wiki documenta `vso.wiki` para lectura y búsqueda; la verificación de autoría mediante Git requiere lectura de código/metadatos (`vso.code`). No cambiar el OAuth de Teams para resolver acceso a Azure DevOps.

Validar formatos reales de `projectWiki` y `codeWiki`: ID, proyecto, repositorio Git, `mappedPath` y versión publicada. Comprobar cómo se relaciona el `path` de Search con el `gitItemPath` de la API de páginas, incluyendo espacios y carpetas de código publicadas. Conservar muestras sintéticas de esas respuestas para regresiones; los datos privados de la cuenta no pasan a tests ni documentación pública.

La URL de organización y los proyectos proceden del catálogo administrado por el operador. Solo construir peticiones a `dev.azure.com` y `almsearch.dev.azure.com`; no seguir URLs devueltas por documentos, el modelo o resultados sin validar su origen y alcance.

## 2. Fuente de conocimiento independiente

Añadir una variante de herramienta, no ampliar silenciosamente la fuente de actividad. Reutilizar el catálogo TOML existente sin añadir campos que rompan su `deny_unknown_fields`. El recurso Wiki referencia el mismo checkout/archivo de catálogo y el mismo secreto si resulta suficiente.

Esquema propuesto de la herramienta:

```toml
[resources.tool]
type = "azure_devops_wiki"
repository = "personal"
path = "integrations/azure-devops.toml"
secret_ref = "secret://azure-devops/reader"
wiki_ids = []
author_mode = "prefer_mine"
```

Los valores de este ejemplo son sintéticos. `wiki_ids=[]` significa todas las wikis accesibles dentro de los proyectos del catálogo; una lista restringe adicionalmente esa fuente. El ID sugerido del recurso es `azure-devops-wikis`, pero el ejecutor identifica la capacidad por su tipo y no exige ese nombre.

Modos: `prefer_mine` prioriza entre resultados relevantes las páginas con creación o contribución propia verificada; `mine_only` incluye creación **o edición** propia verificada; `all` conserva el orden de relevancia nativo. Un override de consulta cambia el criterio de selección, nunca el alcance autorizado ni las reglas de autoridad, atribución y enlaces obligatorios.

El alta usa `pta sources add` y comienza deshabilitada, sin audiencias y sin procesamiento externo. Las audiencias de Wiki se autorizan de forma independiente; no copiar `*` ni los permisos de la fuente de actividad. `sources show/edit/enable/disable/audience` y `config apply/validate` administran el recurso con el esquema compartido.

## 3. Buscar, recuperar y verificar autoría

1. Ejecutar el POST de búsqueda de solo lectura en `almsearch.dev.azure.com`, con `searchText`, `$top`, `$skip` y el filtro documentado `Project` cuando los proyectos sean explícitos. Si el catálogo usa `projects=["*"]`, consultar Search a nivel de organización, sin recorrer todos los proyectos antes de buscar.
2. Verificar en código que cada resultado pertenece al proyecto y wiki autorizados. Restringir por `wiki_ids` después de la búsqueda si no hay un filtro nativo documentado que lo garantice. No confiar únicamente en el filtro de la API.
3. Normalizar cada resultado a organización, proyecto, wiki, repositorio, ruta Git, versión publicada y enlace canónico de página Wiki. Deduplicar por organización/proyecto/wiki/ruta/versión. No transformar ingenuamente una ruta `.md` en ruta de Wiki: resolverla con los metadatos de la API y verificar `mappedPath` en `codeWiki`. Una página sin enlace Wiki resoluble queda fuera de la evidencia de respuesta y se informa cobertura parcial.
4. Recuperar Markdown real mediante Wiki Pages (`includeContent=true`), usando la ruta canónica. Si Search proporciona una ruta Git, la lectura de Git Items existente puede resolverla dentro del repositorio y carpeta publicados, pero la referencia de respuesta sigue siendo el enlace Wiki verificado. No presentar highlights de Search como si fueran el documento completo ni sustituir el enlace Wiki obligatorio por un enlace Git.
5. Consultar historial Git solo para los candidatos relevantes. Usar la identidad configurada y confirmar el correo devuelto, como hace la actividad actual. No aplicar el límite temporal de la actividad. Clasificar `created_by_me`, `edited_by_me`, `other` solo cuando haya evidencia suficiente; de lo contrario, `unknown`. Mantener separadas la evidencia de contribución propia y la identidad/fecha/rol del autor o último editor registrado que se usará para atribuir documentación de terceros.
6. La creación requiere un commit con adición del archivo y una historia coherente con la versión publicada. Un commit propio de edición solo prueba contribución. El primero de una página de resultados no demuestra creación; imports, renombres, squash, ramas ambiguas o historia truncada quedan sin atribución de creación hasta verificarla. `showOldestCommitsFirst` ignora los descriptores de versión según la documentación; no usarlo para atribuir sin comprobar la rama.
7. Aplicar el modo de autoría y seleccionar páginas por relevancia temática. La preferencia personal no debe colocar una página propia ajena al tema delante de la documentación pertinente. En `mine_only`, una página sin creación o edición propia verificable no pasa el filtro. La ausencia de una contribución propia en una historia truncada no prueba que el usuario nunca haya participado.

Como creación **o edición** propia bastan para la política del usuario, buscar primero un commit propio verificable sobre la ruta y la versión publicada; no recorrer toda la historia para encontrar al creador si ya se probó una contribución. Consultar por separado el último editor cuando haga falta atribuir una fuente de tercero. Si se investiga la creación, acotar a 100 entradas de historia por candidato y señalar cualquier límite alcanzado.

No se pretende examinar todo el historial de todas las wikis por pregunta. Presupuesto inicial: páginas de Search de 25 candidatos, hasta 100 candidatos globales por consulta, historial de hasta 10 candidatos y hasta cuatro páginas de contenido para el contexto del modelo. Concurrencia máxima de cuatro peticiones de lectura, sin tareas desacopladas del plazo de la operación.

Límite de 1 MB por respuesta/documento y contexto sujeto al `max_context_chars` existente. Timeout global inicial de 30 segundos para búsqueda Wiki; las llamadas individuales usan un plazo menor o el tiempo restante. Mantener esta operación por debajo del timeout IPC de 120 segundos y comprobar el recorrido del chat con sus proveedores. No copiar el timeout de 180 segundos de la actividad, que ya supera el del cliente IPC.

La paginación limitada y un timeout con resultados parciales deben devolver `partial=true` y qué fase quedó incompleta. Verificar `infoCode`: indexación pendiente, consulta inválida y recorte de resultados no equivalen a cero coincidencias. Ante un fallo solo de Git se puede responder con contenido verificable y autoría `unknown`; ante fallo de lectura no afirmar que se leyó la página. Un 401/403 no activa un fallback que amplíe el alcance ni use contenido privado guardado anteriormente.

Empezar con contenido leído en cada consulta. Si la verificación de autoría requiere cache, usar la tabla existente con claves que incluyan identidad/perfil, alcance del catálogo, organización/proyecto/wiki/repositorio, ruta y revisión comprobada. No almacenar Markdown completo, consultas humanas ni PAT. Los cambios de revisión invalidan la atribución anterior; un dato cacheado no sustituye la comprobación vigente de acceso.

## 4. CLI y contrato de salida

Añadir estos comandos al CLI y al despachador compartido:

```text
pta [--json] azure wiki list SOURCE_ID
pta [--json] azure wiki search SOURCE_ID
pta [--json] azure wiki read SOURCE_ID
```

`search` y `read` reciben JSON por stdin. Para búsqueda: `{"query":"procedimiento de despliegue","wiki_id":null,"author_mode":"prefer_mine"}`. Para lectura: `{"wiki_id":"ID","path":"/Procedimiento"}`; `path` es la ruta canónica de Wiki retornada por la búsqueda. `wiki_id` y la ruta se resuelven dentro de la fuente elegida; nunca se acepta una organización o URL arbitraria en la entrada.

`list` permite inspeccionar los metadatos del catálogo de una fuente registrada aunque esté deshabilitada, para completar su configuración. `search/read` exigen una fuente habilitada y selección explícita por el operador local. Devuelven datos redactados sin invocar modelos ni enviar mensajes a Graph. `external_processing` se exige cuando el contenido vaya a Jev/LLM, como en `pta chat` y `test simulate`; los comandos de consulta local no amplían audiencias Teams.

Conservar contrato JSON 1 y sus códigos de salida; añadir campos a `data`, sin cambiar el envelope. Resultado de búsqueda: consulta normalizada, cobertura, `partial`, avisos y páginas con proyecto/wiki/título/ruta/enlace/revisión, clasificación de autoría, nombre verificado y rol de atribución (creador o último editor) y extractos. No imprimir correos de autores ni secretos. Cero resultados es una operación exitosa; permisos ausentes, red y datos inválidos usan los códigos existentes apropiados, con mensajes saneados.

`read` devuelve el contenido redactado y su referencia de origen. Los comandos son la forma de inspeccionar y diagnosticar la integración antes de generar respuestas. La ayuda, `capabilities` y la referencia de la skill incorporada deben documentar las nuevas operaciones.

## 5. Integración en respuestas

Registrar la fuente Wiki entre las herramientas autorizadas. Ajuste solicitado por el usuario el 2026-10-01 tras una prueba real: Jev clasifica intención ambigua, selecciona referencias después de redactar con decisiones tipadas y valida la respuesta final; no selecciona fuentes ni veta la recuperación antes de buscar. Una petición explícita sobre Wiki tiene prioridad frente al atajo `status_question()`: «Según la wiki, ¿cómo se configura el pipeline?» ejecuta Wiki aunque contenga «pipeline». «¿Qué he hecho esta semana?» mantiene la consulta de actividad. La selección se hace entre recursos autorizados; si hay varias fuentes Wiki y el alcance no es inequívoco, seleccionar mediante el asistente entre IDs autorizados o pedir que se concrete el proyecto.

Construir `searchText` a partir de la solicitud actual, eliminando el prefijo de búsqueda y acotando/escapando los términos según la sintaxis nativa. No enviar como consulta el bloque completo de historial ni la respuesta anterior. Una continuación sin tema propio puede reutilizar la última pregunta humana como referencia; si sigue siendo ambigua, pedir el tema en vez de consultar texto de relleno. El CLI acepta además una consulta explícita para reproducir la búsqueda sin depender de esta normalización.

Cada pasaje enviado al modelo conserva un ID de fuente, wiki, título, URL, revisión y datos de autoría/atribución. El recorte genérico de `excerpt()` no debe separar los párrafos de su referencia; construir bloques autocontenidos por página y aplicar la selección dentro de cada bloque antes de repartir el presupuesto. Conservar también esos metadatos tipados en el pipeline, sin recuperarlos analizando texto del modelo. Redactar antes de elegir/truncar contenido, como en el pipeline actual.

Para conservar el contrato `ReadOnlyTool -> String`, las variantes Wiki y Azure afectadas pueden devolver un resultado JSON serializado con evidencia y referencias, que el pipeline deserializa antes de construir los pasajes. Los comandos directos usan esos mismos resultados tipados. Las herramientas restantes conservan su contrato; las referencias no se reconstruyen analizando prosa del modelo.

Extender `GeneratedAnswer` con `used_sources`, una lista de IDs de fuentes y entidades que respaldan la respuesta, incluidas páginas Wiki, work items, pipelines y stages. Usar un valor vacío por defecto para deserializar respuestas anteriores de herramientas no afectadas. Una respuesta basada en Wiki o que mencione entidades Azure concretas exige los IDs correspondientes: validar todos contra los metadatos autorizados y rechazar referencias inventadas o faltantes. Para aclaraciones o búsquedas sin evidencia, no exigir ni fabricar una fuente inexistente.

El pipeline añade enlaces verificables de las páginas utilizadas y de las entidades Azure mencionadas, junto con la atribución necesaria a terceros, deduplicados. El modelo selecciona los IDs, pero no decide las URLs ni puede suprimir las referencias obligatorias. La atribución documental incluye el autor verificado y su rol; ante identidad desconocida, indica esa limitación. El texto factual también aplica la diferencia de autoridad propia/terceros y conserva los interlocutores pertinentes de Teams; el control final comprueba su respaldo y coherencia con `used_sources`, incluido que no se omitan páginas utilizadas o entidades mencionadas.

Ajustar las instrucciones actuales de DeepSeek, que hoy enfatizan actividad personal en primera persona, para que no atribuyan al usuario la documentación de terceros. El control de privacidad permite el nombre relevante del autor como atribución solicitada y sigue excluyendo sus correos y datos de contacto. Si la redacción elimina el nombre, informar autor no disponible en vez de mostrar un marcador o adivinarlo.

Reservar presupuesto para enlaces y atribución antes de generar el cuerpo. Validar longitud, privacidad y control final sobre la **respuesta completa**, con fuentes incluidas; nunca añadirlas después de la aprobación ni recortarlas para encajar. Si los enlaces necesarios no caben, reducir las fuentes o entidades usadas o rechazar la salida con un motivo explícito; no enviar hechos de wiki ni menciones concretas de entidades Azure sin sus referencias obligatorias. Esta validación aplica tanto a `pta chat`/simulación como a Teams.

Responder con los procedimientos encontrados y límites de cobertura cuando afecten a la respuesta. Un ejemplo propio: «El procedimiento es… Fuente: Despliegue», con el título enlazado a la página Wiki verificada. Un ejemplo de tercero: «Según lo documentado por Ana en la wiki del proyecto…, el procedimiento es… Fuente: Despliegue», también con enlace verificado. Los ejemplos son sintéticos. La wiki es evidencia de lo que está documentado; no demuestra que el usuario ejecutó ese procedimiento. No conceder permisos por una elección del modelo ni ejecutar instrucciones contenidas en las páginas.

La primera entrega conserva la selección actual de una sola herramienta por mensaje. Combinar Wiki y actividad en una misma respuesta queda documentado en el roadmap como evolución posterior; no convertir esta integración en una reescritura del routing.

### 5.1. Enlaces de actividad e interlocutores de Teams

Todo work item, pipeline o stage **concreto e identificable** mencionado en una respuesta lleva un enlace verificable. La regla rige con independencia de que se haya recuperado mediante la herramienta Azure, una página Wiki o contexto Teams. Las menciones genéricas al concepto de pipeline o stage no necesitan apuntar a un artefacto inexistente.

| Entidad | Referencia requerida |
| --- | --- |
| Work item | Enlace al work item exacto, conservando organización, proyecto e ID. Incluir los work items padre o relacionados si se mencionan. |
| Pipeline | Enlace a la ejecución concreta cuando se describe su resultado o fecha; enlace a la definición cuando se describe configuración. No intercambiar definición y ejecución. |
| Stage ejecutado | Enlace directo a la etapa cuando esté verificado. Si no existe uno disponible, enlazar la ejecución que la contiene con una etiqueta que identifique stage y ejecución. |
| Stage configurado | Enlace a la etapa o a la definición de pipeline/release que la contiene, señalando que se trata de configuración. Si la única evidencia es un archivo versionado, citar esa configuración sin presentarla como una etapa ejecutada. |

Capturar estas referencias durante la lectura de Azure, a partir de enlaces web devueltos por la API o de rutas de interfaz verificadas con organización/proyecto/IDs conocidos. Conservar el padre de cada stage, el ID de ejecución/definición y, cuando proceda, la revisión. Las identidades de referencias incluyen el tipo, organización, proyecto y contexto de ejecución; dos stages con el mismo nombre o dos IDs iguales en proyectos distintos no comparten identidad por su nombre.

Preferir el nombre o ID de la entidad enlazado en su primera mención, con deduplicación de referencias posteriores. Validar que las entidades mencionadas están cubiertas por `used_sources` y por enlaces del registro autorizado. Si falta un enlace verificable, señalar la limitación o excluir esa afirmación de la respuesta; no inventar la URL ni emitir la entidad concreta sin su referencia. Resolver enlaces faltantes únicamente dentro del alcance autorizado, sin ampliar permisos para completar una cita.

En el contexto Teams, conservar por mensaje autor verificado, nombre visible, fecha, conversación/mensaje y qué intervención respaldó el hecho. Distinguir al usuario conectado de sus interlocutores y preservar mensajes cercanos relevantes cuando demuestren la interacción. Mantener estos datos al redactar y seleccionar pasajes, de modo que una frase no termine atribuida a la persona del pasaje anterior. No añadir una lectura global de miembros o chats para enumerar personas.

La respuesta nombra a las personas cuando explique una coordinación, una petición, una observación, una decisión o una confirmación relevante. Ejemplos sintéticos: «Coordiné la revisión con Ana» solo si los mensajes prueban esa interacción; «Luis indicó que faltaba la validación» solo si esa intervención es de Luis. La pertenencia al grupo o un mensaje aislado de una persona no prueban que el usuario interactuó con ella. No enumerar participantes ajenos al hecho ni mostrar correos, teléfonos o identificadores privados.

Si falta el nombre, fue redactado o la identidad es ambigua, usar una atribución limitada como «otra persona del equipo» y señalar la falta de identificación cuando importe. Los nombres de interlocutores pertinentes están autorizados como contexto de trabajo; el control final comprueba quién dijo qué y que el resumen no transforma comentarios de terceros en acciones o compromisos del usuario. La conversación puede respaldar que se habló de un despliegue; demostrar que se ejecutó conserva los requisitos de evidencia de ejecución existentes.

## 6. Verificación y criterios de cierre

Regresiones con `wiremock` ya disponible, centradas en fronteras reales:

- Wiki de proyecto y Wiki de código con carpeta/versiones publicadas; rutas con espacios, Unicode, `%` y subpáginas, sin escapes de `mappedPath`.
- Resultados de otros proyectos/wikis rechazados aunque la API los devuelva; fuente no autorizada no ejecutada; comandos locales no modifican audiencias.
- Creación propia y edición propia habilitan autoridad documental; `mine_only` admite ambas y rechaza identidad inferida. Autoría ajena/desconocida se atribuye sin inventar creador ni confundirlo con el último editor.
- Paginar y declarar cobertura limitada; reindexación, 401/403/404/429, timeout y documentos mayores de 1 MB; errores sin cuerpos privados ni tokens.
- Pregunta sobre Wiki que contiene «pipeline» ejecuta Wiki; actividad reciente mantiene su ruta; extractos y citas conservan la página de origen.
- Respuesta propia con enlace, respuesta de tercero con autor/ubicación/enlace, respuesta mixta con todas sus fuentes y autor desconocido señalado. Ausencia de IDs, ID inventado, enlace fuera del alcance y falta de espacio para citas impiden enviar una respuesta factual sin fuentes; una aclaración sin evidencia no crea citas falsas.
- Cada work item citado —incluidos padres/relacionados—, pipeline y stage lleva el enlace correspondiente. Cubrir entidades obtenidas de Azure, Wiki y Teams; distinguir ejecución/configuración, stages homónimos y alcance por organización/proyecto. Un enlace faltante o manipulado no se rellena inventándolo.
- Contexto Teams con dos interlocutores, mensajes propios y de terceros: se menciona a cada persona cuando corresponde, sin intercambiar autores ni asumir interacción por pertenencia al grupo. Cubrir nombres ausentes/redactados y verificar que la selección de pasajes mantiene autor y hecho juntos.
- Comandos JSON, stdin y validación del esquema; lectura local sin llamadas a LLM/Graph; simulación con la fuente Wiki sin envío a Graph.

Después, prueba real desde CLI con una página conocida por el usuario: localizarla, leer su Markdown actual y comprobar enlace, versión y autoría cuando exista evidencia. La prueba local no necesita enviar una respuesta a Teams. Verificar que una modificación de la página se refleja en una nueva lectura y que el límite de autoría/cobertura se expone en casos incompletos.

Cierre de implementación: nueva fuente validable/configurable por CLI; `list/search/read` funcionales; simulaciones comprueban autoridad propia, atribución de terceros, enlaces obligatorios de Wiki/work items/pipelines/stages e interlocutores relevantes de Teams, con al menos una respuesta respaldada por una página real; regresiones pertinentes pasan; documentación de operación y roadmap actualizados. La GUI y una publicación de binarios no son criterios de cierre. Registrar con qué compilación compatible de CLI/host se verificó y qué versiones distribuidas siguen sin soporte.

## Referencias primarias

- [Wiki Search REST 7.1](https://learn.microsoft.com/en-us/rest/api/azure/devops/search/wiki-search-results/fetch-wiki-search-results?view=azure-devops-rest-7.1): búsqueda, paginación, resultados y estados de indexación.
- [Wikis List REST 7.1](https://learn.microsoft.com/en-us/rest/api/azure/devops/wiki/wikis/list?view=azure-devops-rest-7.1): catálogo de wikis, tipos, repositorios, carpeta publicada y versiones.
- [Wiki Pages Get REST 7.1](https://learn.microsoft.com/en-us/rest/api/azure/devops/wiki/pages/get-page?view=azure-devops-rest-7.1): contenido, rutas Git, enlaces y ETag.
- [Git Commits REST 7.1](https://learn.microsoft.com/en-us/rest/api/azure/devops/git/commits/get-commits?view=azure-devops-rest-7.1): identidad, historia por ruta, versiones y paginación.
