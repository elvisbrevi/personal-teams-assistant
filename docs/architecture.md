# Arquitectura y controles

| Módulo | Responsabilidad |
| --- | --- |
| `adapters` | OAuth, Graph, webhooks y conversión de Teams a `IncomingMessage`; `MessageAdapter` es la frontera del canal |
| `decision` | API HTTP Jev y validación de `Choice`; no redacta texto |
| `knowledge` | Mapa TOML, audiencias, lectura acotada, selección de pasajes y HTTP público |
| `llm` | Trait `LlmProvider`, implementación DeepSeek mediante Rig |
| `tools` | Trait `ReadOnlyTool`, herramientas semánticas y `ScopedTool` compatible con Rig |
| `security` | Resolución de secretos, cifrado, redacción y comparaciones de secretos |
| `state` | SQLite: cola, deduplicación, auditoría, suscripciones y tokens cifrados |
| `pipeline` | Orquestación independiente de APIs de Teams |

## Estados e idempotencia

`pending → processing → ignored | dry_run | sending → sent | uncertain`.

La clave primaria es la ruta canónica del mensaje, incluyendo chat/canal. Webhooks solo verifican y encolan antes de responder `202`. El worker reintenta lecturas/proveedores hasta cinco intentos, con backoff y filtro de antigüedad. La cola se recupera al arrancar; un lock de archivo impide dos procesos sobre el mismo estado.

Antes del envío, se vuelve a leer el mensaje y se verifica que no haya cambiado ni caducado. Se persiste `sending` con SQLite `synchronous=FULL` **antes** del POST. Graph no ofrece una garantía de idempotencia de mensajes: timeout, caída durante envío o respuesta inválida producen `uncertain`, que nunca se reenvía automáticamente. Al reiniciar, los `sending` quedan `uncertain`. Esto prioriza evitar duplicados y puede omitir una respuesta. No se promete entrega exactamente una vez.

Un único worker simplifica concurrencia y auditoría; el límite es throughput y latencia. Si la cola envejece, los mensajes quedan ignorados por antigüedad. Para aumentar escala, agregar leases por cuenta y control de rate limits sin eliminar la barrera persistente previa al envío.

## Jev

Cada llamada usa `POST https://api.typesafe.ai/v1/systemone`, `model`, `state` y preguntas `Choice` independientes para decisión y seguridad. La confianza efectiva es la menor de ambas. Se valida tipo, opción permitida, rango finito, distribución y que la opción elegida tenga probabilidad máxima. Respuestas incompletas o desconocidas fallan de forma cerrada.

El routing contiene candidatos autorizados y las salidas `ignore`/`human`; evidence y final gate contienen `allow`/`ignore`/`human`. El LLM solo recibe pregunta, evidencia limitada y estilo configurado; no el mapa completo ni los secretos de herramientas. Las preguntas con PII detectada quedan manuales.

Jev y las instrucciones al LLM reducen riesgos de prompt injection, pero no demuestran ausencia de ataques. Ninguna decisión de modelo concede permisos: audiencias, rutas, acciones, consultas SQL y URLs se verifican en código antes de ejecutar. Los resultados de herramientas y documentos se presentan como datos no confiables. El LLM no tiene ejecución autónoma de herramientas.

## Secretos y datos sensibles

Redacción antes de seleccionar/truncar pasajes, por coincidencia exacta de secretos cargados, patrones de tokens, credenciales comunes, correo, teléfonos/números largos y RUT. `sensitive_patterns` agrega reglas propias del dominio. Los nombres personales, datos empresariales y formatos nuevos no se detectan universalmente: deben minimizarse en las fuentes y declararse con patrones/restricciones de audiencia. El gate final añade una segunda evaluación pero no reemplaza autorización.

El resultado propuesto que contiene patrones sensibles se bloquea. Auditoría guarda la versión redactada; no se registran cuerpos HTTP, credenciales, prompts ni respuestas de error externas. Solo están habilitados logs propios con eventos fijos; se desactiva tracing de proveedores, que podría incluir prompts.

El directorio de estado tiene permisos `0700` y contiene datos redactados de auditoría y tokens cifrados con nonce aleatorio y contexto autenticado. Respaldar DB y clave por separado. Implementar una política de retención de auditoría conforme al uso; el MVP conserva registros hasta que el operador los elimine. No borrar claves de deduplicación recientes sin aceptar reprocesamiento de notificaciones.

## Límites y pendientes explícitos

- Solo una identidad Teams por instancia.
- Cobertura de nuevas conversaciones comienza tras el siguiente descubrimiento/suscripción; no hay garantía de todos los mensajes durante apagones.
- Recuperación de `missed` acotada a la página reciente. Para historial completo implementar paginación/cursor por conversación con política explícita de no responder mensajes antiguos.
- No detecta todavía si el usuario ya contestó manualmente mientras se genera una respuesta; revisar dry-run antes de activar y mantener ventanas cortas.
- Canales, SQL Server, ADO, RabbitMQ y HTTP están implementados pero requieren pruebas contra tus endpoints, permisos y esquemas reales antes de habilitarlos.
- DeepSeek es el único proveedor registrado. OpenAI/Anthropic/Gemini/local requieren una implementación de `LlmProvider` y su registro; el pipeline no cambia.
- Los checkouts Git se actualizan de forma explícita con `git pull --ff-only` y revisión; no se ejecutan hooks ni comandos extraídos de documentos.
- Túnel temporal no equivale a alta disponibilidad. Hostname estable, supervisión y equipo encendido son pendientes operativos para servicio continuo.

Referencias: [Jev HTTP](https://docs.typesafe.ai/api), [confianza](https://docs.typesafe.ai/confidence), [Rig](https://docs.rs/rig-core/0.32.0/rig/).
