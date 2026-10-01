# Agentes de este proyecto

Cuando el usuario pida guardar, corregir u organizar información en la base de conocimiento, sigue [save-knowledge](.agents/skills/save-knowledge/SKILL.md). Usa el checkout declarado en `knowledge-map.toml`.

Para configurar, operar o diagnosticar la aplicación, sigue [personal-teams-assistant](desktop/src-tauri/skills/personal-teams-assistant/SKILL.md).

La prioridad de entrega es siempre la aplicación CLI (`pta`) y su núcleo compartido. Si la GUI o los binarios distribuidos quedan desfasados de funcionalidades, documenta la diferencia en [el roadmap](docs/roadmap.md); su paridad no bloquea el trabajo del CLI. No incluyas actualización de GUI, reinstalación ni publicación de binarios como tareas obligatorias salvo solicitud explícita del usuario.

Para la funcionalidad Wiki del asistente, la documentación creada o editada por el usuario puede respaldar una respuesta con su autoridad. La documentación de terceros requiere indicar dónde está la información y quién la documentó, sin inventar autoría ausente. Toda respuesta basada en wikis incluye enlaces a las páginas usadas como fuentes. El [plan Wiki](docs/azure-devops-wiki-plan.md) detalla estos controles.

Las respuestas del asistente también incluyen enlaces a cada work item, pipeline y stage concreto que mencionen. Si usan información de conversaciones de Teams, nombran a las personas con quienes se interactuó cuando sea relevante y esté respaldado por los mensajes, conservando quién dijo o hizo qué. No inventar nombres ni atribuir una interacción a todos los miembros del chat.
