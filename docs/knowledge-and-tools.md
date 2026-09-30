# Conocimiento y herramientas

## Git y audiencias

Clonar los repositorios privados usando SSH o un credential helper de Git. Montarlos de solo lectura donde sea posible. El mapa es configuración del operador; nunca se carga del mensaje recibido. Cada archivo debe vivir dentro de un checkout declarado, con extensión aprobada. Se bloquean rutas absolutas, `..` y symlinks que escapen del root. No se ejecuta contenido de archivos.

Cada fuente requiere `enabled=true`, `external_processing=true` y una conversación exacta en `allowed_conversations`. `allowed_senders` restringe además quién puede preguntar. Autorizar un grupo implica autorizar la divulgación al **grupo completo**. Observar todos los chats no concede acceso a todas las fuentes.

El mapa contiene descripciones, temas, capacidades (mediante el tipo y operación) y forma de acceso. Se leen los documentos accesibles al remitente/conversación, con un máximo de 1 MB por documento. Tras redactar, se seleccionan hasta cuatro pasajes por documento; todos comparten el límite total `max_context_chars`. Jev recibe solo el catálogo de herramientas autorizadas cuando hace falta elegir una operación de lectura. Su incertidumbre no descarta la pregunta: se responde con los documentos disponibles o se pide aclaración. No se manda el repositorio completo.

Para guardar hechos nuevos o corregir los existentes, los agentes siguen [save-knowledge](../.agents/skills/save-knowledge/SKILL.md). Cada repositorio privado define su propia organización de documentos temáticos y los descriptores de las fuentes.

Archivos: Markdown/TXT y documentos JSON/TOML/YAML se tratan como texto. No se incluyen PDF, imágenes ni Office en esta versión. Los roots adicionales se añaden a `[repositories]`; el pipeline no cambia.

URLs: exactas, HTTPS, puerto 443, sin credenciales. Se resuelve DNS, se rechazan IP privadas/reservadas y se fija la resolución en el cliente para reducir DNS rebinding. No se siguen redirects. APIs internas usan herramientas explícitas porque sus hosts privados son una autorización diferente. Aplicar también política de egreso de red si el equipo procesa información sensible.

## SQL Server

Configurar un login dedicado **sin permisos de escritura** y vistas mínimas en el schema `assistant_readonly`. TLS obligatorio con certificado verificable; no se admite `TrustServerCertificate`. El timeout incluye conexión, consulta y lectura: cinco segundos. Máximo 1 fila para una búsqueda y 20 para integraciones fallidas.

| Operación | Vista y columnas |
| --- | --- |
| `get_payment_status` | `assistant_readonly.payments(payment_id, status)` |
| `find_transaction` | `assistant_readonly.transactions(transaction_id, status)` |
| `get_failed_integrations` | `assistant_readonly.failed_integrations(integration_name, status)` |

IDs se extraen de la sintaxis explícita `id: ABC-123`, deben ser alfanuméricos/guion/underscore y se pasan como parámetros TDS `@P1`. Se requiere exactamente un ID. No se interpola SQL. Crear las vistas y conceder `SELECT` debe hacerlo el dueño de la BD según su modelo real; no se incluye una migración que suponga tablas de negocio.

```toml
[[resources]]
id = "payments"
description = "Consulta el estado de un pago identificado por id: VALOR"
topics = ["pago", "estado"]
enabled = true
external_processing = true
allowed_conversations = ["chats/CHAT-ID-APROBADO"]
kind = "tool"
[resources.tool]
type = "sql_server"
operation = "get_payment_status"
secret_ref = "secret://sqlserver/payments-readonly"
```

En `config.toml`:

```toml
[secrets]
"secret://sqlserver/payments-readonly" = "SQLSERVER_READONLY_CONNECTION_SECRET"
```

SQL dinámico está deshabilitado incluso para `SELECT`. Si alguna vez se incorpora, necesita parser SQL real, una sentencia, allowlists de schemas/tablas, límites, timeout y auditoría; no basta una expresión regular.

## Otras integraciones

Reutilizar el bloque de recurso anterior cambiando `[resources.tool]`:

```toml
# Work item por id: NUMERO, lectura de System.Id, System.Title y System.State.
type = "azure_devops"
organization = "ORGANIZACION"
project = "PROYECTO"
secret_ref = "secret://azure-devops/reader"
```

Usar un PAT con Work Items **Read** para el proyecto correspondiente.

```toml
# Metadatos de una cola; no consume ni publica mensajes.
type = "rabbitmq"
url = "https://rabbit.example.com/api/queues/%2F/payments"
secret_ref = "secret://rabbitmq/monitor"
```

El secreto tiene formato `usuario:password`; cuenta con permisos mínimos de monitoreo. El resultado solo expone nombre, estado, cantidades y consumidores.

```toml
# Solo GET sobre una URL exacta fijada por el operador, sin redirecciones.
type = "http"
url = "https://internal.example.com/health/business"
secret_ref = "secret://internal/status"
```

El header es Bearer; omitir `secret_ref` si la API no lo necesita. No incluir tokens en query strings. La respuesta tiene límite de 64 KB, redacción y los mismos gates antes de llegar a Teams. Esta herramienta asume un GET sin efectos laterales: revisar el endpoint antes de autorizarlo.
