# Personal Teams Assistant Desktop

App de bandeja/barra de menús (Tauri 2) y CLI administrativo del asistente personal de Microsoft Teams. Un solo paquete de Cargo instala:

- `personal-teams-desktop`: GUI y host del servicio (también corre oculto con `--host`).
- `pta`: CLI que controla el mismo host por un canal loopback autenticado.
- La skill operativa para agentes, incorporada en `pta` (`pta skill show|path|install`).

```sh
cargo install personal-teams-desktop --locked
personal-teams-desktop
pta --help
```

Requiere Rust, los prerrequisitos nativos de Tauri y el directorio `bin` de Cargo en `PATH`. Para Linux o servidores, `cargo install personal-teams-desktop --no-default-features --locked` instala el host sin interfaz (`personal-teams-desktop --headless --start`) y `pta`, sin Tauri ni WebKit. GUI y CLI comparten el perfil `dev.personalteams.assistant`, el directorio de datos y las credenciales del almacén del sistema; reinstalar los conserva.

Arquitectura y configuración: README y `docs/architecture.md` del repositorio.
