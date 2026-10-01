`pta start`, `pta stop` y `pta restart` administran el asistente; `pta status` (sin `--json`) resume si el host y el asistente corren, el modo, Teams y los modelos. `pta help` describe todos los comandos.

**Actualizar.** `cargo install personal-teams-desktop --locked` reemplaza los binarios, pero el host anterior sigue corriendo hasta el primer uso de la versión nueva: el primer `pta` o abrir la app lo detiene (asistente y túnel). Si el asistente corría, la terminal pregunta si dejarlo corriendo con la versión nueva; con `--json`, `--non-interactive` o sin terminal se conserva el estado anterior (vuelve a iniciarse); la app muestra un aviso con «Dejar corriendo» / «Dejarlo detenido». Verifica después con `pta status`. Un host lanzado desde otra ruta no se detiene así. Inicio y parada repetidos son seguros. El host puede vivir oculto sin servicio iniciado; `pta app open` muestra su ventana, `pta app hide` la oculta y `pta app quit` termina el host después de parar el servicio y su túnel. Una CLI corta puede terminar dejando el host/servicio solicitado ejecutándose.

`pta --json status` distingue configuración persistida y cargada; una versión anterior sin canal de control debe cerrarse desde su GUI antes de iniciar la nueva. Un listener abierto por sí solo no demuestra autorización o recepción de Teams. Consulta también la salud de suscripciones y auditoría.

`pta tunnel configure token`, `pta tunnel configure file PATH` o `pta tunnel configure external` eligen un modo. El token se guarda como `CLOUDFLARE_TUNNEL_TOKEN` con credentials set, nunca en argumentos. `pta tunnel validate` verifica el modo y los requisitos locales, y no demuestra conectividad desde Internet.

El canal administrativo usa otro puerto loopback, token de instancia en un archivo privado y bloqueo de perfil. El túnel Graph conserva únicamente su origen configurado. Cambiar la configuración valida primero; un reinicio fallido exige inspeccionar si quedó aplicado y detenido. Tras caída abrupta, el próximo host recupera la transacción pendiente de configuración; las salidas ambiguas de Graph no se reenvían.

**Host sin interfaz (Linux/servidores).** Se instala con `cargo install personal-teams-desktop --no-default-features --locked` (sin Tauri/WebKit) o se fuerza con `personal-teams-desktop --headless` / `PTA_HEADLESS=1`. `pta --json status` muestra `headless: true`; `app open/hide` responden `not_ready`; `app quit`, SIGTERM o Ctrl-C detienen servicio y túnel antes de salir. `--start` intenta iniciar el servicio al arrancar y mantiene vivo el host si falla, para diagnosticarlo. En Linux las credenciales se guardan con `pta credentials set NAME` en `~/.config/dev.personalteams.assistant/credentials/default/` (0600) o se entregan como `NAME`/`NAME_FILE` en el entorno del host. Login remoto: `pta auth microsoft login --no-browser`, autorizar la URL en cualquier dispositivo y pegar la dirección `http://localhost:PUERTO/?code=…` que el navegador no pudo abrir en `pta auth microsoft finish --redirect 'URL'`. Servicio de usuario systemd en `~/.config/systemd/user/pta.service` (`systemctl --user enable --now pta`, con `loginctl enable-linger` para que siga sin sesión):

```ini
[Unit]
Description=Personal Teams Assistant (headless)
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=%h/.cargo/bin/personal-teams-desktop --headless --start
Restart=on-failure
RestartSec=10

[Install]
WantedBy=default.target
```

Nunca mantener activas a la vez dos instancias de la misma cuenta (otro equipo, la GUI local o un servidor): duplicarían respuestas y provocarían bucles en el chat personal. Deja la otra detenida o en `pta mode observe`.
