# Roadmap

## Prioridad de entrega

Decisión del usuario registrada el 2026-09-30: la prioridad es siempre la aplicación CLI (`pta`) y el núcleo que ejecuta sus operaciones. Las diferencias de funcionalidades con la GUI y los binarios distribuidos se documentan aquí. Corregirlas o publicar una nueva distribución no es un requisito para cerrar una funcionalidad del CLI, salvo petición explícita.

`pta` usa actualmente el host de escritorio por IPC autenticado. Implementar una operación del CLI puede requerir cambiar ese host compartido, sin añadir controles a la GUI. Un ejecutable instalado previamente no adquiere las funcionalidades del código nuevo: para usarlas necesitará una versión compatible del CLI y del host. No aplicar esquemas nuevos a un perfil servido por un host que todavía no los admite.

## Wikis y referencias de actividad Azure/Teams

| Entrega | Estado | Alcance y criterio de cierre |
| --- | --- | --- |
| Búsqueda y lectura desde el CLI | Implementada en 0.3.0; verificada el 2026-09-30 | APIs nativas, catálogo y credencial existentes, `list/search/read`, rutas/versiones verificadas y prioridad a creación **o edición** propia. [Evidencia y límites de verificación](azure-devops-wiki-verification.md). |
| Uso de wikis en respuestas del asistente | Implementado y corregido el 2026-10-01 en 0.3.0 | Fuente independiente y audiencias propias; autoridad documental propia, atribución de terceros/desconocidos y enlaces obligatorios. Preguntas de uso/funcionamiento consultan Wiki sin veto previo de Jev. La [evaluación de Jev](jev-evaluation.md) documenta falsos rechazos, clasificación de intención y validación final selectiva; simulaciones reales de dos temas pasaron sin envío a Teams. |
| Enlaces de actividad Azure y atribución de Teams | Implementados en la misma entrega del CLI | Metadatos verificables y validación completa antes de aprobar. Work items, pipelines y stages concretos requieren sus enlaces, distinguiendo ejecución y configuración. Teams conserva interlocutor e intervención; regresiones cubren autores intercambiados y nombres ausentes. |
| Configuración y pruebas de herramientas en la GUI | Pendiente; no bloquea el CLI | La GUI actual filtra recursos `kind=file` al renderizar fuentes y la selección del chat. Añadir soporte visual para la nueva fuente Wiki cuando se solicite. |
| Binarios publicados con soporte Wiki | Pendiente de publicación; no bloquea el CLI | Compilación verificada: `target/debug/pta` y `target/debug/personal-teams-desktop` 0.3.0, contrato 1. La distribución instalada observada sigue en 0.2.0 y carece de Wiki. Usar CLI/host compatibles; no aplicar la fuente nueva al host antiguo. |
| Observación de una edición real de Wiki | Pendiente de acción humana | La regresión controlada comprueba nueva revisión/contenido y evita atribución obsoleta. No se modificó una página real durante la prueba de lectura. Para observarlo, el operador edita una página autorizada y repite `read`; no se declara realizada esa comprobación externa. |
| Actividad y Wiki en una misma respuesta | Evolución posterior | El pipeline actual selecciona una sola herramienta por mensaje. Considerar selección acotada de varias fuentes si se solicita combinar ambas operaciones. |

Los campos históricos `follow_up_threshold`, `routing_threshold` y `evidence_threshold` se conservan por compatibilidad de configuración; el pipeline ya no usa esas etapas. El umbral `final_threshold` sigue activo. La clasificación remota de intención ambigua exige una selección válida con confianza ≥0.50 y no recibe fuentes.

## Otros pendientes de distribución

Windows conserva los pendientes ya descritos en [la documentación del CLI](cli.md#roadmap-windows-próxima-versión): instalador conjunto, actualización de CLI/PATH, pruebas funcionales, firma y ciclo de desinstalación. El [plan de escritorio](desktop-app-plan.md) conserva el detalle histórico del empaquetado y de la GUI.
