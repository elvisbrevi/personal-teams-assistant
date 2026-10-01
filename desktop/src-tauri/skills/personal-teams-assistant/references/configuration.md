Consulta `pta config show` o `pta config get policy.dry_run`. `pta config set policy.dry_run true` conserva los demás campos y valida antes de aplicar. El último argumento es JSON, incluyendo comillas para strings. Todos los campos existentes se pueden editar por su ruta con puntos; objetos, listas y campos opcionales se reemplazan como JSON completo.

`pta config apply` y `pta config validate` leen JSON o TOML de stdin con `config`, `map` y, opcionalmente, `tunnel_config`. Parte de una consulta reciente. Las escrituras basadas en una consulta detectan cambios de revisión; tras conflicto, vuelve a leer y rehace el cambio. La GUI también participa del mismo control. Validación e importación conservan las restricciones del mapa y de identidad.

`pta mode observe` y `pta mode active` persisten y aplican el modo al servicio existente; verifica `pta mode show`. Los jobs de observación permanecen deduplicados.

`pta credentials list` devuelve nombres y procedencia. `pta credentials set NAME` lee el valor por stdin, limitado a 16 KiB; debe estar detenido el servicio. La allowlist viene de la configuración. La clave de una base cifrada existente se conserva. El agente puede aceptar el acceso a credenciales existentes necesario para la operación autorizada, según la regla de permisos de [la skill](../SKILL.md). Primero reutiliza las ubicaciones auditadas; nunca imprime secretos. Si el control del diálogo está bloqueado por la herramienta o macOS exige autenticación presencial, informa ese impedimento concreto y pide únicamente la intervención necesaria, sin solicitar otra credencial.

`pta config import FILE` requiere servicio detenido, resuelve rutas relativas contra el archivo importado y mantiene el directorio de datos y la clave existentes. Una base no se reutiliza con otra identidad Microsoft.

`policy.max_answer_chars` limita respuestas normales; `policy.max_detailed_answer_chars` limita las detalladas. DeepSeek elige el modo en su salida tipada; la app cuenta caracteres Unicode y aplica el límite antes del control final y el envío. La API admite un presupuesto de tokens y JSON, mientras la longitud exacta se verifica localmente.
