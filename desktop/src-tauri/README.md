# Personal Teams Assistant Desktop

Aplicación de barra de menús o bandeja para el asistente personal de Teams. La interfaz, los iconos y la configuración de Tauri están dentro de este paquete de Cargo.

Desde un checkout de este workspace:

```sh
cargo install --path desktop/src-tauri --locked
personal-teams-desktop
```

Una vez publicados `personal-teams-assistant` y `personal-teams-desktop` en crates.io, la instalación desde el registro será `cargo install personal-teams-desktop`.

El binario instalado por Cargo funciona con ventana y bandeja, pero no crea por sí solo un paquete `.app`/`.dmg` o instalador Windows. Para esos paquetes usa `cargo tauri build` desde este directorio. Consulta el README principal del repositorio para la configuración y el modelo de credenciales.
