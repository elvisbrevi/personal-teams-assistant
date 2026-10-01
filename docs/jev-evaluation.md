# Evaluación de Jev y selección de conocimiento

Evaluación del 2026-10-01 motivada por una pregunta documental real en el chat propio. La auditoría mostró `source=assistant-operations`, `tools=[]` y confianza de selección 0.37: Wiki nunca se consultó. La respuesta de aclaración sí pasó el control final. Una búsqueda nativa posterior encontró la página requerida y verificó contribución propia. No se envió ningún mensaje de prueba a Teams.

## Selección antes de buscar

Se evaluaron nueve consultas con los descriptores y el umbral 0.50 del perfil existente. No se cambiaron permisos ni umbrales. Los nombres privados de recursos y páginas no se incluyen aquí.

| Intención esperada | Casos | Resultado del selector anterior |
| --- | ---: | --- |
| Documentación: uso, funcionamiento, integración, parámetros, ubicación y Wiki explícita | 6 | Tres permitidos y tres rechazados antes de buscar; confianza de los rechazos 0.29, 0.36 y 0.38. |
| Actividad: trabajo semanal y estado de pipeline | 2 | Ambos seleccionaron actividad. |
| Crear una HU | 1 | Se dejó para intervención humana. |

La mitad de las consultas documentales de esta muestra sufrió un falso rechazo. En dos de ellas Jev incluso seleccionó Wiki, pero su confianza impidió consultarla. Esta muestra diagnostica el fallo; no es una estimación estadística de todas las preguntas futuras.

Decisión solicitada por el usuario: antes de consultar conocimiento solo se distingue pregunta/solicitud de información, saludo o mensaje informativo. Preguntas y saludos claros no necesitan clasificación remota; los casos ambiguos usan `Stage::Intent` sin catálogo de fuentes. Una pregunta permite recuperar conocimiento autorizado. Jev ya no selecciona herramientas ni juzga si una página aún no leída contiene la respuesta.

La selección usa rutas de capacidad conocidas o DeepSeek con un conjunto cerrado de IDs autorizados. Wiki explícita y consultas de uso/configuración preceden a las palabras de actividad; una Wiki única sirve como lectura documental si no se pide actividad ni otra capacidad. Se conserva una sola herramienta por respuesta. Audiencias, habilitación, procesamiento externo, catálogo, plazo y límites siguen comprobados en código. Una intención ambigua sin fuente autorizada no se envía al clasificador.

## Control final

El control anterior rechazó una de tres respuestas sintéticas válidas: interpretó con duda `edited_by_me` con `author=null`, pese a que esos metadatos verifican contribución propia. También rechazó respuestas reales ya citadas por ese mismo control, con confianza 0.59–0.62. Se aclaró el contrato y se retiró la reclasificación probabilística de la atribución Wiki construida por el código. Las afirmaciones de autoría, creación o ejecución del cuerpo siguen contrastadas con la evidencia en `supported`; la atribución conversacional conserva su comprobación específica cuando hay mensajes Teams.

El umbral final permanece en 0.65. La prueba reproducible `cargo run --example wiki_gate_smoke`, con la credencial existente, evalúa hechos sintéticos y no construye un cliente Graph:

| Caso | Resultado tras el ajuste | Confianza |
| --- | --- | ---: |
| Procedimiento genérico citado | Aceptado | 0.86 |
| Contribución propia con autor nulo intencional | Aceptado | 0.67 |
| Fuentes propias y de terceros con atribución verificada | Aceptado | 0.80 |
| Stage concreto sin referencia | Rechazado | 0.48 |
| Ejecución inventada a partir de un procedimiento | Rechazado | 0.06 |
| Promesa personal nueva | Rechazado | 0.12 |
| Último editor presentado como creador | Rechazado | 0.10 |
| Autores Teams intercambiados | Rechazado | 0.63 |

Tres mensajes ambiguos adicionales se clasificaron correctamente: petición de información (0.96), saludo (0.98) e información sin pregunta (0.95). Los resultados pueden variar entre llamadas; las regresiones locales fijan el contrato, permisos y comportamiento del código.

## Selección tipada de referencias

El usuario solicitó delegar el formato de referencias en Jev. Tras la redacción, el núcleo envía una pregunta Noul por entrada del registro autorizado (`source_0`, `source_1`, etc.). Jev indica qué fuentes respaldan el texto; el código convierte esas decisiones a los IDs exactos ya registrados. No se solicita que ningún modelo reconstruya una URL ni copie un prefijo. DeepSeek devuelve únicamente texto y nivel de detalle. El contrato rechaza campos ausentes/extra, tipos incorrectos, probabilidades fuera de rango y decisiones fuera del conjunto; el control final recibe la respuesta con sus citas incorporadas. La prueba real sintética seleccionó correctamente una página aun con un identificador sugerido sin prefijo.

Se reutilizan las [decisiones tipadas de TypeSafe](https://typesafe.ai/); que el formato esté restringido no prueba por sí solo el respaldo de un hecho, por lo que se conserva la validación final.

Los IDs de páginas son ahora `wiki:` más SHA-256 de organización/proyecto/wiki/versión/ruta, presentados explícitamente como `id` en el contexto. Son identidades estables y compactas del registro; no se corrigen por aproximación ni se aceptan IDs inventados. El registro conserva URL, revisión, ubicación y autoría completas. El contexto usa también el título canónico para reconocer nombres espaciados y compactos, conserva más contenido útil y evita mezclar instrucciones de versiones distintas.

Las simulaciones reales finales desde `pta --json test simulate`, con la selección tipada, consultaron Wiki y pasaron el control final. La pregunta de uso de un microservicio produjo 3358 caracteres/tres páginas, respuesta detallada dentro del límite 8000 y confianza final 0.66. La pregunta sobre funcionamiento de notificaciones de pago produjo 1205 caracteres/una página, respuesta normal dentro del límite 3000 y confianza 0.72. Todos los IDs usados pertenecen al registro y todos sus enlaces aparecen en la respuesta; la primera conserva contribución propia y atribución de terceros. Declararon cobertura parcial por los límites de búsqueda/contexto. Sus respuestas pertenecen al adaptador local `simulation-only`, sin envío a Graph. La [verificación Wiki](azure-devops-wiki-verification.md) registra el recorrido del CLI/host compatible y la comprobación de servicio.
