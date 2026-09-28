# Local y despliegue

## Modo local integrado

```sh
# macOS
brew install cloudflared azure-cli
az login
python3 scripts/local.py
```

El script compila, carga secretos con `lazy-workflow credentials-get` en memoria, crea un Quick Tunnel y actualiza el redirect URI de la app configurada. Usa el tenant y cuenta con permisos de modificar esa app en Azure CLI. El servicio escucha solo en loopback; Graph entra por el túnel HTTPS. La URL actual queda en `data/local-url.txt`.

Mantén abierta la terminal del script mientras uses el asistente. Consulta la URL vigente con `cat data/local-url.txt` y abre `<URL vigente>/test` para usar el simulador. Pulsa `Ctrl-C` en esa terminal para detener juntos el servicio y el túnel. Vuelve a ejecutar `python3 scripts/local.py` para iniciarlos otra vez. Si el script quedó en segundo plano, identifica su PID con `pgrep -fl 'scripts/local.py'` y usa `kill -TERM <PID>`; el script termina también sus procesos hijos.

Para recuperar la clave de administración en tu propia terminal sin ponerla en argumentos:

```sh
lazy-workflow credentials-get --name ADMIN_AUTH_KEY --force --no-log-file | pbcopy
```

Pégala en el formulario `/oauth/login` y completa Microsoft con el usuario configurado. Este comando es para la terminal del usuario, no para logs/CI. En Windows/Linux usa tu gestor de secretos y evita que el valor aparezca en historial.

Al reiniciar el túnel se registra una URL nueva. Las suscripciones deben apuntar a la URL actual; el servicio reconcilia o renueva las propias. Si el equipo duerme o el túnel expira, reinicia el script. Los mensajes atrasados no disparan respuestas masivas: se aplica `max_message_age_seconds`.

Para funcionamiento continuo desde el mismo equipo:

1. Usar un túnel con nombre y hostname estable sobre tu futuro dominio, o un reverse proxy HTTPS accesible.
2. Configurar `server.public_url` y el callback Entra con ese hostname.
3. Ejecutar el servicio Rust y el túnel con `launchd`, systemd o un servicio Windows, con reinicio ante fallos.
4. Mantener el equipo encendido y con Internet; los webhooks no pueden llegar a un equipo apagado.
5. Respaldar el volumen SQLite y la clave de cifrado por separado; supervisar eventos `subscription_sync_failed`, `processing_failed` y filas `uncertain`.

El dominio propio queda pendiente; no se requiere cambiar Rust ni migrar el estado cuando se disponga de él. [Documentación oficial de Quick Tunnels](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/trycloudflare/).

## Contenedor

```sh
# Preparar config.toml y knowledge-map.toml para paths del contenedor.
# server.bind = "0.0.0.0:3000", data_dir = "/app/data"
# repositories.personal = "/knowledge/personal"
docker compose up --build -d
```

El compose de desarrollo lee `.env` local, monta configuración y conocimiento de solo lectura y persiste SQLite en un volumen. Para producción reemplazar `.env` por mounts de secretos y variables `NAME_FILE`, mantener usuario no-root y terminar TLS en el proxy/túnel. No montar el socket Docker ni el directorio completo de secretos.

El Dockerfile usa build multietapa y una imagen runtime sin compilador. CI construye la imagen; no requiere credenciales ni cuentas de Teams para los tests. Un solo contenedor por directorio de estado.

## Operación y revisión

```sh
# No contiene tokens; muestra estado y razones de decisiones.
sqlite3 data/assistant.db 'SELECT status,count(*) FROM jobs GROUP BY status;'
sqlite3 data/assistant.db 'SELECT audit FROM jobs WHERE status IN ("dry_run","uncertain","failed") ORDER BY created_at DESC LIMIT 20;'
```

Auditoría está redactada, pero debe tratarse como información privada. Nunca consultar/imprimir la tabla `vault`. `/healthz` es liveness, no prueba consentimiento ni conectividad completa. Para readiness operacional verificar al menos una suscripción vigente y ausencia de errores persistentes.

Antes de activar envíos: autorizar las audiencias del conocimiento, revisar propuestas en dry-run y enviar mensajes de prueba desde otra cuenta a un chat autorizado. Cambiar `dry_run=false` no reprocesa mensajes anteriores. Para volver a modo observación, establecerlo a `true` y reiniciar.

Rotar el secreto Entra antes de caducar. Si se cambia `GRAPH_WEBHOOK_SECRET`, eliminar/recrear únicamente las suscripciones de esta app; conservar la clave anterior hasta completar el cambio evita perder notificaciones. Rotar `STATE_ENCRYPTION_KEY` requiere re-cifrar tokens o volver a autorizar; no sustituirla sin un plan de recuperación.
