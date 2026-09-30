`pta doctor --offline` valida archivos locales sin abrir la base en modo de recuperación ni llamar a proveedores. `pta doctor` inspecciona configuración, credenciales y conexión; su resultado indica expresamente que no probó red. Los errores/pendientes tienen exit code distinto de cero.

`pta chat` o `pta test simulate` reciben SimulationRequest JSON por stdin: session, text, group, mentioned, sources. Usa fuentes habilitadas que permitan procesamiento externo. La simulación llama a los proveedores configurados y puede tener coste; nunca envía mensajes a Graph ni amplía audiencias.

`pta test self-chat` comprueba membresía de la conversación configurada. No constituye prueba de envío ni de recepción. Para el cierre real, la persona escribe una nueva pregunta allí y verifica su respuesta. Consulta `pta audit list --limit 20` y `pta self-chat status`. Repite tras reiniciar. Las respuestas del asistente deben quedar ignoradas sin producir otro envío.

`pta audit show RESOURCE` y `pta logs --limit 50` son consultas de lectura. La auditoría omite proposed/sent por defecto; `--content` es una consulta privada explícita. Los archivos de publicación y reportes compartidos contienen metadatos y resultados, sin mensajes personales ni credenciales.

`pta test providers` prueba Jev y DeepSeek con hechos ficticios, incluyendo selección de respuesta normal/detallada y sus límites; consume API. `pta test connectivity` valida la cuenta Graph conectada mediante lectura, sin envío.
