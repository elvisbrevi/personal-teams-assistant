`pta start`, `pta stop` y `pta restart` administran el asistente. Inicio y parada repetidos son seguros. El host puede vivir oculto sin servicio iniciado; `pta app open` muestra su ventana, `pta app hide` la oculta y `pta app quit` termina el host después de parar el servicio y su túnel. Una CLI corta puede terminar dejando el host/servicio solicitado ejecutándose.

`pta status` distingue configuración persistida y cargada; una versión anterior sin canal de control debe cerrarse desde su GUI antes de iniciar la nueva. Un listener abierto por sí solo no demuestra autorización o recepción de Teams. Consulta también la salud de suscripciones y auditoría.

`pta tunnel configure token`, `pta tunnel configure file PATH` o `pta tunnel configure external` eligen un modo. El token se guarda como `CLOUDFLARE_TUNNEL_TOKEN` con credentials set, nunca en argumentos. `pta tunnel validate` verifica el modo y los requisitos locales, y no demuestra conectividad desde Internet.

El canal administrativo usa otro puerto loopback, token de instancia en un archivo privado y bloqueo de perfil. El túnel Graph conserva únicamente su origen configurado. Cambiar la configuración valida primero; un reinicio fallido exige inspeccionar si quedó aplicado y detenido. Tras caída abrupta, el próximo host recupera la transacción pendiente de configuración; las salidas ambiguas de Graph no se reenvían.
