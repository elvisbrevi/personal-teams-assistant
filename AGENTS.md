# Guía para agentes

Lee primero [la arquitectura](docs/architecture.md): explica componentes, pipeline, datos, credenciales, invariantes y recetas de cambio.

## Qué skill usar

- Guardar, corregir u organizar hechos de la base de conocimiento: [save-knowledge](.agents/skills/save-knowledge/SKILL.md).
- Configurar, operar o diagnosticar la aplicación instalada: [personal-teams-assistant](desktop/skills/personal-teams-assistant/SKILL.md) (también `pta skill show`).

## Prioridades

- El producto es un solo paquete de Cargo, `personal-teams-assistant`: la GUI y host `personal-teams-assistant`, el CLI `pta` y su skill. El mismo binario corre sin interfaz (`--headless`, o compilado con `--no-default-features` para Linux). No hay otros crates, bundles `.app`/`.dmg`/instaladores ni servidor independiente.
- Tauri solo se usa en `src/app/gui.rs` (recursos en `desktop/`); las operaciones usan `Host`/`Shell` y deben compilar con y sin la feature `gui`.
- La prioridad de entrega es el CLI y su núcleo compartido. Si la GUI queda desfasada, anótalo en «Límites conocidos» de la arquitectura; su paridad no bloquea el trabajo del CLI. No reinstales, publiques ni actualices la GUI salvo que el usuario lo pida.

## Invariantes que no se pueden romper

- Conservar perfil `dev.personalteams.assistant`, servicio de Llavero `personal-teams-assistant.default`, nombres de credenciales, `data_dir` y `STATE_ENCRYPTION_KEY`: de ellos depende que la sesión Microsoft y las credenciales sigan funcionando sin pedirlas otra vez.
- No quitar ni renombrar campos de `Config`/`KnowledgeMap` (`deny_unknown_fields`); añadir solo con `#[serde(default)]`.
- No añadir scopes de Graph ni cambiar el flujo OAuth (cliente público + PKCE + loopback).
- Nunca reintentar un envío a Graph; ante duda, `uncertain` y revisión humana.
- Las decisiones de modelos no conceden permisos: audiencias, rutas, URLs y límites se verifican en código.
- Fuentes nuevas nacen deshabilitadas, sin audiencias y sin `external_processing`.
- No imprimir ni registrar secretos; credenciales solo por stdin o almacén del sistema.

## Reglas de las respuestas del asistente

- Wiki: la documentación creada o editada por el usuario puede respaldar una respuesta con su autoridad. La de terceros indica dónde está y quién la documentó, sin inventar autoría ausente. Toda respuesta basada en wikis enlaza las páginas usadas.
- Cada work item, pipeline y stage concreto mencionado lleva su enlace verificado.
- Con información de conversaciones de Teams, nombrar a las personas con quienes se interactuó cuando sea relevante y esté respaldado, conservando quién dijo o hizo qué. No inventar nombres ni atribuir una interacción a todos los miembros del chat.

## Antes de entregar

No hay CI (GitHub Actions está desactivado para no generar costos): estas verificaciones, en local, son la única barrera. No añadas workflows de GitHub Actions.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo test
cargo test --no-default-features
```

Actualiza la arquitectura y la skill si cambias comportamiento, comandos o esquemas.
