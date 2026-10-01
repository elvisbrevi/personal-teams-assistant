# Mejoras propuestas

Propuestas surgidas al revisar el código completo (2026-10-01). Ninguna está implementada. Prioridad: **P1** alto valor y bajo riesgo, **P2** valor claro con más esfuerzo, **P3** evolución. Cada punto indica dónde está el problema para retomarlo sin volver a investigar.

## 1. Arquitectura

### P1 — Operaciones del host tipadas y separadas de Tauri
- **Problema:** `desktop/src-tauri/src/lib.rs` (≈1.300 líneas) mezcla UI de bandeja, arranque del servicio, OAuth, túnel, ajustes y Wiki. `control::operate` despacha por strings (`"start_assistant"`) y los códigos de salida se deducen de prefijos de texto (`"[invalid_input] …"`, `"[not_ready] …"`).
- **Propuesta:** módulo `host` sin dependencia de Tauri con `enum Operation` (serde) y `enum HostError { InvalidInput, NotReady, Conflict, Network, Pending, Failed }` que mapee a exit codes. Tauri e IPC quedan como adaptadores finos.
- **Beneficio:** pruebas de operaciones sin ventana, menos errores al añadir comandos, mensajes coherentes GUI/CLI.

### P1 — Fábrica única de proveedores
- **Problema:** Jev, DeepSeek y el cliente HTTP se construyen igual en `runtime.rs`, `local_chat.rs` y `diagnostics.rs`, con endpoints fijos repetidos (`https://api.typesafe.ai/v1/systemone`, `https://api.deepseek.com`).
- **Propuesta:** `Providers::from_config(&Config)` que devuelva gate, llm, redactor y cliente; endpoints opcionales en `[jev]`/`[llm]` con `#[serde(default)]`.

### P2 — Pipeline por etapas con estados tipados
- **Problema:** `Pipeline::process_with_sources` (`src/pipeline.rs`) es una función de ~400 líneas; estados y motivos son strings libres; la herramienta de actividad se reconoce por el ID fijo `"azure-devops-status"`.
- **Propuesta:** etapas `triage → retrieve → generate → cite → gate → deliver`, cada una con un resultado tipado; `enum JobStatus` y `enum Reason` serializados igual que hoy (compatibilidad de auditoría); enrutar por tipo de `ToolSpec`, no por ID.
- **Beneficio:** cada etapa testeable por separado y motivos estables para la GUI.

### P2 — CLI fuera del crate de Tauri
- **Problema:** `pta` vive en el mismo crate que la GUI, así que compila y enlaza Tauri/WebView aunque solo hace HTTP loopback.
- **Propuesta:** crate `pta-protocol` (Request/Reply/contrato) compartido; `pta` como crate propio sin Tauri. Mantener `cargo install personal-teams-desktop` instalando ambos (dependencia o workspace con dos paquetes publicados).
- **Beneficio:** compilación mucho más rápida del CLI y posibilidad de CLI en Linux.

### P2 — Host sin interfaz y soporte Linux
- **Problema:** el host exige un proceso Tauri; Linux no tiene almacén de credenciales (`keyring` solo macOS/Windows en `Cargo.toml`).
- **Propuesta:** modo `--host --headless` sin WebView y backend Secret Service en Linux. Permitiría un servicio `launchd`/systemd para mantener la recepción tras reiniciar sesión.

### P2 — Esquema de configuración versionado
- **Problema:** `jev.follow_up_threshold/routing_threshold/evidence_threshold` y `graph.channels` ya no se usan, pero deben persistir porque hosts antiguos que comparten el perfil los exigen (`deny_unknown_fields`).
- **Propuesta:** `version = 2` en `config.toml` con migración explícita y respaldo, aplicada cuando no queden hosts 0.2.x. Entonces eliminar los campos heredados.

### P3 — Modularizar `src/ado.rs` (≈1.600 líneas) y `src/ado/wiki.rs` (≈1.850)
- Separar catálogo, Git/commits, pipelines/stages, releases, contexto Teams y referencias; los tests ya existentes se mueven con cada módulo.

## 2. Funcionamiento del asistente

### P1 — Recepción automática y decisión de participación
- **Problema:** el uso normal todavía depende de `discover_all_chats` o de una lista manual `allowed_chats` (textarea «Chats permitidos» en la GUI); no existe una decisión explícita de «intentar / guardar silencio / dejar a la persona» para cada mensaje elegible. Detalle en el handoff no versionado `docs/handoffs/2026-09-30-respuesta-por-contexto.md`.
- **Propuesta:** `discover_all_chats = true` por defecto con migración observable; ampliar `Stage::Intent` (o una etapa `Participation`) con resultados `attempt | ignore | human`, reutilizando las decisiones tipadas de Jev; mostrar el motivo en GUI/CLI.

### P1 — Métricas por etapa en la auditoría
- **Problema:** no se registra cuánto tarda cada llamada (Graph, herramienta, DeepSeek, Jev ×N); diagnosticar latencia o coste exige reproducir.
- **Propuesta:** duraciones y número de llamadas en `Audit`; `pta audit stats` con percentiles por motivo y fuente.

### P2 — Varias fuentes de herramienta por respuesta
- **Problema:** una sola herramienta por mensaje; «qué hice esta semana y cómo se configura X según la wiki» pierde una parte.
- **Propuesta:** hasta N herramientas autorizadas con presupuesto de contexto compartido y plazo total, manteniendo el registro de referencias por fuente.

### P2 — Enrutamiento menos dependiente del idioma
- **Problema:** `question_request`, `documentation_question` y `status_question` son listas de prefijos/palabras en español e inglés; frases nuevas caen en la ruta equivocada.
- **Propuesta:** que la clasificación de intención devuelva también la capacidad buscada (`documentation | activity | other`) cuando hay varias herramientas, conservando las heurísticas como atajo barato y el conjunto cerrado de IDs.

### P2 — Menos llamadas a Jev en la selección de referencias
- **Problema:** `Jev::select_references` envía una pregunta `noul` por referencia (hasta 100).
- **Propuesta:** preseleccionar en código las referencias cuyo título/ID aparece en el texto y consultar a Jev solo las dudosas, o agruparlas en una `choice` multi-respuesta.

### P2 — Detectar respuesta manual del usuario antes de enviar
- **Problema:** si el usuario contesta mientras se genera la propuesta, el asistente igualmente envía (límite conocido).
- **Propuesta:** en la relectura previa al envío, listar los mensajes posteriores del chat y abortar si hay uno propio humano.

### P3 — Recuperación completa de `missed`
- Cursor por conversación para paginar más allá de la página reciente, con la política de no responder mensajes antiguos.

### P3 — Retención de auditoría
- `jobs.audit` y `events` crecen sin límite. Política configurable (p. ej. 90 días) que conserve las claves de deduplicación recientes, e índices `jobs(status, next_at)`.

## 3. Stack y dependencias

- **P1 — Reconsiderar `rig-core`.** Solo se usa como cliente DeepSeek y para la fachada `ScopedTool` (`src/tools.rs`), que no aporta llamada autónoma a herramientas. DeepSeek es compatible con la API de OpenAI: una llamada `reqwest` con `response_format=json_object` elimina una dependencia grande. Mantenerla solo si se planean varios proveedores vía Rig.
- **P2 — Features de Cargo para integraciones opcionales.** `tiberius` (SQL Server), RabbitMQ y HTTP genérico detrás de features (`sql-server`, `rabbitmq`), activadas por defecto si se quiere compatibilidad. Reduce tiempo de compilación y superficie.
- **P2 — Migraciones SQLite versionadas** con `PRAGMA user_version` en lugar de solo `CREATE TABLE IF NOT EXISTS`.
- **P3 — Tipos compartidos Rust→JS** (`ts-rs`/`specta`) para que `ui/app.js` no se desincronice del `Snapshot`/`Config`.

## 4. GUI

- **P1 — Fuentes de cualquier tipo.** `renderResources()` en `ui/app.js` filtra `kind=file`: la Wiki y la actividad de Azure DevOps solo se administran por CLI. Añadir alta/edición de herramientas, editor de audiencias y conmutadores habilitar/procesamiento externo.
- **P1 — Estado operativo real.** Panel con suscripciones activas y su caducidad, último mensaje recibido, últimos resultados de auditoría con motivo legible, salidas `uncertain` o pendientes de reconciliar y estado del túnel.
- **P2 — Icono de bandeja con estado** (sin configurar / detenido / activo / requiere atención) y notificación del sistema ante `uncertain`, fallo de suscripción o login caducado.
- **P2 — Inicio automático con la sesión** (opción desactivada por defecto; `tauri-plugin-autostart`): la recepción depende de que el host esté vivo.
- **P2 — Chat de prueba más informativo:** permitir probar sin fuente seleccionada, mostrar referencias como enlaces, cobertura parcial y motivo humanizado.
- **P3 — Visor de auditoría** con filtro por estado/motivo y contenido solo bajo petición explícita (como `pta audit show --content`).

## 5. Seguridad y operación

- **P1 — Firma de código estable en macOS.** Cada `cargo install` produce un binario sin firma estable; el Llavero puede volver a preguntar «Permitir acceso» (las credenciales no se pierden). Firmar localmente con un certificado propio estable tras instalar (o un script `pta`-asistido) mantiene la ACL entre versiones.
- **P2 — Límite de tasa y concurrencia en el listener público** (`tower` `ConcurrencyLimit`/`RateLimit` en `webhook::router`); hoy cualquier origen que alcance el túnel puede forzar validaciones y lecturas de la base.
- **P2 — Ruta absoluta configurable de `cloudflared`** en lugar de buscar en `/opt/homebrew`, `/usr/local` y `PATH` (`lib.rs::start`).
- **P2 — Log local rotativo** con los mismos eventos fijos que ya emite `tracing`, para diagnosticar tras un cierre del host (hoy solo hay tabla `events`).
- **P3 — Rotación guiada de `STATE_ENCRYPTION_KEY`** (re-cifrado de `vault`) y de `GRAPH_WEBHOOK_SECRET` (recrear suscripciones propias).

## 6. Calidad y pruebas

- **P1 — Comprobar el JavaScript en CI** (`node --check desktop/src-tauri/ui/app.js` como mínimo).
- **P2 — Pruebas del host sin Tauri** una vez extraídas las operaciones (arranque/parada, revisión, journal de ajustes, protección de identidad y clave).
- **P2 — Pruebas de propiedades** (`proptest`) para `teams::canonical_resource`, `knowledge` (rutas) y `Redactor`.
- **P3 — CLI en Linux en CI** cuando `pta` deje de depender de Tauri.

## 7. Distribución

- **P1 — Publicar 0.3.0.** El repositorio está en 0.3.0 (Wiki, referencias verificadas) y la versión en crates.io es anterior. Orden: `personal-teams-assistant` y luego `personal-teams-desktop`; usar `scripts/export-public.py` si se publica desde un snapshot sin historial privado.
- **P2 — Windows funcional:** probar bandeja, Credential Manager, ACL e inicio/parada antes de anunciarlo.
