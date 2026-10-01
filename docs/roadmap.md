# Roadmap

## Prioridad de entrega

Decisión del usuario registrada el 2026-09-30: la prioridad es siempre la aplicación CLI (`pta`) y el núcleo que ejecuta sus operaciones. Las diferencias de funcionalidades con la GUI y los binarios distribuidos se documentan aquí. Corregirlas o publicar una nueva distribución no es un requisito para cerrar una funcionalidad del CLI, salvo petición explícita.

`pta` usa actualmente el host de escritorio por IPC autenticado. Implementar una operación del CLI puede requerir cambiar ese host compartido, sin añadir controles a la GUI. Un ejecutable instalado previamente no adquiere las funcionalidades del código nuevo: para usarlas necesitará una versión compatible del CLI y del host. No aplicar esquemas nuevos a un perfil servido por un host que todavía no los admite.

## Wikis y referencias de actividad Azure/Teams

| Entrega | Estado | Alcance y criterio de cierre |
| --- | --- | --- |
| Búsqueda y lectura desde el CLI | Planificada | [Plan de implementación](azure-devops-wiki-plan.md): catálogo accesible, búsqueda nativa, lectura de Markdown y prioridad a páginas con autoría propia verificable. |
| Uso de wikis en respuestas del asistente | Planificado | Fuente independiente, autorizaciones existentes, autoridad propia para páginas creadas/editadas por el usuario y atribución explícita a terceros. Toda respuesta basada en Wiki incluye enlaces a las páginas utilizadas; validación de fuentes antes del envío. |
| Enlaces de actividad Azure y atribución de Teams | Planificado como parte de la misma entrega del CLI | Cada work item, pipeline y stage concreto mencionado lleva enlace verificable. La información extraída de Teams conserva quién dijo qué y nombra a los interlocutores cuando sea relevante y verificable. |
| Configuración y pruebas de herramientas en la GUI | Pendiente; no bloquea el CLI | La GUI actual filtra recursos `kind=file` al renderizar fuentes y la selección del chat. Añadir soporte visual para la nueva fuente Wiki cuando se solicite. |
| Binarios publicados con soporte Wiki | Pendiente de una entrega del CLI | Registrar aquí la versión que incorpore la funcionalidad cuando se distribuya. El CLI observado en esta sesión es `pta 0.2.0`, contrato 1; Wiki todavía no está implementado. |
| Actividad y Wiki en una misma respuesta | Evolución posterior | El pipeline actual selecciona una sola herramienta por mensaje. Considerar selección acotada de varias fuentes si se solicita combinar ambas operaciones. |

## Otros pendientes de distribución

Windows conserva los pendientes ya descritos en [la documentación del CLI](cli.md#roadmap-windows-próxima-versión): instalador conjunto, actualización de CLI/PATH, pruebas funcionales, firma y ciclo de desinstalación. El [plan de escritorio](desktop-app-plan.md) conserva el detalle histórico del empaquetado y de la GUI.
