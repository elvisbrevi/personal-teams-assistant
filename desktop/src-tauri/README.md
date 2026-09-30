# Personal Teams Assistant Desktop

Aplicación de barra de menús o bandeja para el asistente personal de Teams. La interfaz, los iconos y la configuración de Tauri están dentro de este paquete de Cargo.

La instalación principal es mediante Cargo desde crates.io. Instala la GUI y el CLI juntos, con la skill incorporada, sin necesitar un checkout adicional:

```sh
cargo install personal-teams-desktop --version 0.2.0 --locked
personal-teams-desktop
pta --version
```

Requiere Rust y los prerrequisitos nativos de Tauri para compilar. Agrega el directorio `bin` de Cargo (normalmente `~/.cargo/bin`) a `PATH`. La skill se entrega mediante `pta skill show/path/install`. Para desarrollo desde este workspace: `cargo install --path desktop/src-tauri --locked`.

`pta --help` es la referencia de comandos. `pta start/stop/restart` controla el mismo host que la GUI; `pta app open/hide/quit` controla la ventana y el host. El host puede arrancar oculto desde terminal. `pta --json status` muestra estado persistido/cargado sin valores secretos.

Configura desde `config show/get/set/apply`, gestiona credenciales por stdin protegido (el terminal con eco se rechaza), conecta Microsoft/GitHub con `auth`, administra repositorios/fuentes/audiencias y ejecuta `test simulate`, `test providers` o `doctor --offline`. Para Teams personal: `self-chat enable [ID]` valida la cuenta y membresía y conserva audiencias. `policy.max_answer_chars` y `policy.max_detailed_answer_chars` limitan cada modo; el agente elige el modo. `test self-chat` verifica membresía; recepción/envío se comprueban en Teams y audit.

En macOS, el bundle contiene `Contents/MacOS/pta`. `pta app install-cli DIRECTORIO` crea un enlace en un directorio existente elegido de PATH. Roadmap Windows (próxima versión): NSIS incorporará `pta.exe` junto a la GUI; se puede invocar por ruta desde PowerShell o copiar a un directorio elegido mediante app install-cli. La copia Windows debe actualizarse con la app.

La instalación conserva el perfil Tauri `dev.personalteams.assistant`, el directorio de datos importado y las credenciales del sistema. El primer acceso o una firma nueva puede solicitar autorización del sistema. Logout elimina tokens locales, sin revocar consentimiento remoto. Los paquetes locales macOS usan firma ad hoc, sin notarización; CI Windows no sustituye una prueba funcional del producto.

Los binarios precompilados y bundles son alternativas accesorias. La GUI instalada por Cargo funciona con ventana y bandeja; los paquetes `.app`/`.dmg` ofrecen integración adicional con el sistema y se generan con `cargo tauri build` desde este directorio. La notarización macOS queda pendiente y Windows permanece en el roadmap. Consulta el README principal del repositorio para la configuración y el modelo de credenciales.
