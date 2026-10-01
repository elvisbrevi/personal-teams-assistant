# Personal Teams Assistant

Asistente personal de Microsoft Teams escrito en Rust. Lee y responde **con tu propia identidad** (OAuth delegado de Microsoft Graph, sin bot) cuando alguien te escribe directamente, te menciona en un grupo o le preguntas en tu chat personal. Responde solo con evidencia de fuentes que autorizaste para esa conversación —archivos de repositorios Git, URLs, Azure DevOps (actividad y Wiki) y otras herramientas de solo lectura—, redacta con el modelo de lenguaje que elijas —Codex o Claude Code a través de sus CLI instaladas, o la API de DeepSeek, en un orden de respaldo configurable— y valida con Jev (TypeSafe). Si una respuesta no está respaldada o es insegura, no se envía.

## Instalación

Todo se instala con Cargo: la app de bandeja/barra de menús `personal-teams-desktop`, el CLI `pta` y la skill para agentes incorporada en `pta`.

```sh
cargo install personal-teams-desktop --locked          # desde crates.io
cargo install --path desktop/src-tauri --locked        # desde este checkout
```

Requisitos: Rust (versión fijada en `rust-toolchain.toml`), prerrequisitos nativos de Tauri 2, Git y `~/.cargo/bin` en `PATH`. Para recibir mensajes de Teams además necesitas una URL HTTPS estable que llegue al puerto local (p. ej. `cloudflared` con token) y el equipo encendido.

En Linux o en un servidor, instala la variante **sin interfaz** (no necesita Tauri ni WebKit) y opérala solo con `pta`:

```sh
cargo install personal-teams-desktop --no-default-features --locked
personal-teams-desktop --headless --start      # primer plano; apto para un servicio systemd
```

Sus credenciales se guardan con `pta credentials set` en archivos privados del perfil o llegan por variables `NOMBRE`/`NOMBRE_FILE`. El login de Microsoft se completa desde cualquier dispositivo con `pta auth microsoft finish --redirect 'URL'`. Detalles en la [arquitectura](docs/architecture.md#host-sin-interfaz-linux-y-servidores). No mantengas dos instancias activas de la misma cuenta.

GUI y CLI comparten el mismo perfil, credenciales del Llavero/Credential Manager y servicio. Reinstalar no pide credenciales de nuevo: se conservan el perfil `dev.personalteams.assistant`, el directorio de datos y la cuenta Microsoft conectada.

## Uso rápido

```sh
personal-teams-desktop            # abre la ventana (o pta app open)
pta --json status                 # estado del host, servicio y configuración cargada
pta auth microsoft login          # OAuth PKCE con el navegador del sistema
pta auth microsoft finish --wait  # espera a que completes el consentimiento
pta start                         # inicia el servicio Teams (en modo observación si dry_run=true)
pta mode active                   # habilita envíos reales tras revisar las propuestas
pta test simulate <<< '{"session":"demo","text":"¿Qué hice esta semana?","sources":["azure-devops-status"]}'
```

`pta --help` lista todos los comandos; `pta skill show` entrega el manual operativo para agentes.

## Configuración mínima

1. Credenciales (por stdin, nunca como argumento): `TYPESAFE_API_KEY`, `DEEPSEEK_API_KEY` si usas DeepSeek y las referenciadas por tus fuentes, p. ej. `pta credentials set DEEPSEEK_API_KEY < archivo`. `GRAPH_WEBHOOK_SECRET` y `STATE_ENCRYPTION_KEY` se generan solas.
2. Modelos de lenguaje: en la GUI (Configuración → Modelos de lenguaje) activa y ordena Codex, Claude Code y DeepSeek, con su modelo y esfuerzo. Codex y Claude usan la CLI instalada (`codex`, `claude`) con su propia sesión. Por defecto: Codex `gpt-6.1-sol` (medio) → Claude `claude-opus-5-5` (medio) → DeepSeek `deepseek-flash` (máximo). Cada respuesta empieza por el primero y pasa al siguiente solo si falla (p. ej. sin créditos). Desde el CLI: `pta llm providers` y `pta config set llm.chain JSON`.
3. Registro Entra con plataforma *Mobile and desktop* (`http://localhost`) y permisos delegados `User.Read`, `Chat.Read`, `ChatMessage.Send`, `offline_access`. Configura `graph.tenant_id` y `graph.client_id`.
4. URL pública (`server.public_url`) y túnel: `pta tunnel configure token|file PATH|external`.
5. Conocimiento: agrega repositorios (`pta repos add`, o GitHub con `pta auth github login`), registra fuentes con `pta sources add` (nacen deshabilitadas y sin audiencias) y autorízalas por conversación con `pta sources audience`.

## Documentación

- [Arquitectura](docs/architecture.md): componentes, pipeline, datos, seguridad, contrato del CLI y recetas de cambio.
- [Mejoras propuestas](docs/mejoras-propuestas.md).
- [Skill operativa](desktop/src-tauri/skills/personal-teams-assistant/SKILL.md) y [guía para agentes](AGENTS.md).

## Licencia

MIT.
