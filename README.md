# Personal Teams Assistant

Asistente personal de Microsoft Teams escrito en Rust. Lee y responde **con tu propia identidad** (OAuth delegado de Microsoft Graph, sin bot) cuando alguien te escribe directamente, te menciona en un grupo o le preguntas en tu chat personal. Responde solo con evidencia de fuentes que autorizaste para esa conversación —archivos de repositorios Git, URLs, Azure DevOps (actividad y Wiki) y otras herramientas de solo lectura—, redacta con el modelo de lenguaje que elijas —Codex o Claude Code a través de sus CLI instaladas, o la API de DeepSeek, en un orden de respaldo configurable— verifica en código referencias, enlaces y datos sensibles, y registra una revisión de Jev (TypeSafe). Si una respuesta cita algo que no se puede verificar o contiene datos sensibles, no se envía (en tu chat personal te avisa) y queda visible en la pestaña Mensajes.

## Instalación

Un solo paquete de Cargo, `personal-teams-assistant`, instala todo: la app de bandeja/barra de menús y host `personal-teams-assistant`, el CLI `pta` y la skill para agentes incorporada en `pta`.

```sh
cargo install personal-teams-assistant --locked   # desde crates.io
cargo install --path . --locked                   # desde este checkout
```

Si tenías el paquete anterior `personal-teams-desktop` (0.4.0 o antes), desinstálalo primero: ambos instalan `pta` y Cargo no sobrescribe un binario de otro paquete. Tu perfil, credenciales y cuenta se conservan.

```sh
cargo uninstall personal-teams-desktop
cargo install personal-teams-assistant --locked
pta status
```

Requisitos: Rust (versión fijada en `rust-toolchain.toml`), prerrequisitos nativos de Tauri 2, Git y `~/.cargo/bin` en `PATH`. Para recibir mensajes de Teams además necesitas una URL HTTPS estable que llegue al puerto local (p. ej. `cloudflared` con token) y el equipo encendido.

En Linux o en un servidor, instala la variante **sin interfaz** (no necesita Tauri ni WebKit) y opérala solo con `pta`:

```sh
cargo install personal-teams-assistant --no-default-features --locked
personal-teams-assistant --headless --start    # primer plano; apto para un servicio systemd
```

Sus credenciales se guardan con `pta credentials set` en archivos privados del perfil o llegan por variables `NOMBRE`/`NOMBRE_FILE`. El login de Microsoft se completa desde cualquier dispositivo con `pta auth microsoft finish --redirect 'URL'`. Detalles en la [arquitectura](docs/architecture.md#host-sin-interfaz-linux-y-servidores). No mantengas dos instancias activas de la misma cuenta.

GUI y CLI comparten el mismo perfil, credenciales del Llavero/Credential Manager y servicio. Reinstalar no pide credenciales de nuevo: se conservan el perfil `dev.personalteams.assistant`, el directorio de datos y la cuenta Microsoft conectada.

### Actualizar

```sh
cargo install personal-teams-assistant --locked
pta status
```

Cargo no ejecuta nada después de instalar, así que la versión anterior sigue corriendo hasta el primer uso de la nueva. El primer comando `pta` (cualquiera; `pta status` sirve) o abrir la app detecta que el binario instalado cambió, detiene el host anterior (asistente y túnel) y, si el asistente estaba corriendo, pregunta si dejarlo corriendo con la versión nueva (en la app, con un aviso en la ventana). Sin terminal interactiva (`--non-interactive`, `--json`, systemd) conserva el estado anterior.

## Uso rápido

```sh
personal-teams-assistant          # abre la ventana (o pta app open)
pta help                          # todos los comandos, con su descripción
pta status                        # ¿está corriendo? host, asistente, modo, Teams y modelos
pta start                         # inicia el asistente
pta stop                          # lo detiene
pta auth microsoft login          # OAuth PKCE con el navegador del sistema
pta auth microsoft finish --wait  # espera a que completes el consentimiento
pta mode active                   # habilita envíos reales tras revisar las propuestas
pta test simulate <<< '{"session":"demo","text":"¿Qué hice esta semana?","sources":["azure-devops-status"]}'
```

`pta help` (o `pta`, `pta --help`) lista todos los comandos con su descripción; `--json` devuelve el resultado completo en JSON. `pta skill show` entrega el manual operativo para agentes.

## Configuración mínima

1. Credenciales (por stdin, nunca como argumento): `TYPESAFE_API_KEY`, `DEEPSEEK_API_KEY` si usas DeepSeek y las referenciadas por tus fuentes, p. ej. `pta credentials set DEEPSEEK_API_KEY < archivo`. `GRAPH_WEBHOOK_SECRET` y `STATE_ENCRYPTION_KEY` se generan solas.
2. Modelos de lenguaje: en la GUI (Configuración → Modelos de lenguaje) activa y ordena Codex, Claude Code y DeepSeek, con su modelo y esfuerzo. Codex y Claude usan la CLI instalada (`codex`, `claude`) con su propia sesión. Por defecto: Codex `gpt-6.1-sol` (medio) → Claude `claude-opus-5-5` (medio) → DeepSeek `deepseek-flash` (máximo). Cada respuesta empieza por el primero y pasa al siguiente solo si falla (p. ej. sin créditos). Desde el CLI: `pta llm providers` y `pta config set llm.chain JSON`.
3. Registro Entra con plataforma *Mobile and desktop* (`http://localhost`) y permisos delegados `User.Read`, `Chat.Read`, `ChatMessage.Send`, `offline_access`. Configura `graph.tenant_id` y `graph.client_id`.
4. URL pública (`server.public_url`) y túnel: `pta tunnel configure token|file PATH|external`.
5. Conocimiento: agrega repositorios (`pta repos add`, o GitHub con `pta auth github login`), registra fuentes con `pta sources add` (nacen deshabilitadas y sin audiencias) y autorízalas por conversación con `pta sources audience`.

## Documentación

- [Sitio del proyecto](https://elvisbrevi.github.io/personal-teams-assistant/) (fuente en [`site/`](site/), publicada con GitHub Pages).
- [Arquitectura](docs/architecture.md): componentes, pipeline, datos, seguridad, contrato del CLI y recetas de cambio.
- [Mejoras propuestas](docs/mejoras-propuestas.md).
- [Skill operativa](desktop/skills/personal-teams-assistant/SKILL.md) y [guía para agentes](AGENTS.md).

## Licencia

MIT.
