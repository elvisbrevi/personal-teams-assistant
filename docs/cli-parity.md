# Inventario GUI/CLI 0.2.0

La GUI invoca `command` con contrato 1; la consola invoca el mismo despachador por loopback autenticado. El mutex de operaciones serializa ambos adaptadores. La columna de verificación identifica regresiones ejecutables, sin convertirlas en una afirmación de prueba funcional de cada proveedor o plataforma.

| Operación de aplicación | CLI | Verificación pertinente |
| --- | --- | --- |
| snapshot y estado de bandeja | status, doctor, mode show | Lectura SQLite sin recuperar jobs; estado cargado separado del persistido |
| iniciar/detener y menús | start, stop, restart | Lock del perfil y dataset; readiness del listener; cleanup de tareas y túnel |
| mostrar/ocultar/salir | app open/hide/quit | Mismo dueño; CLI desacoplada del grupo de procesos del terminal |
| save_settings y formularios | config show/get/set/apply/validate | Validadores compartidos; revisión del snapshot; journal de recuperación |
| import_existing | config import | Rutas relativas resueltas; identidad y clave de SQLite protegidas; servicio detenido |
| set_credential/delete_credential | credentials list/set/delete | Allowlist del perfil, stdin acotado, metadatos sin valores, protección de clave existente |
| connect_microsoft | auth microsoft status/login/finish/cancel/logout | PKCE, cookie, identidad, refresh cifrado y replay; autorización con timeout |
| begin/open/finish/disconnect GitHub | auth github status/login/finish/cancel/logout | Device Flow existente; logout local; espera cancelable |
| github_repositories/clone/update | github repos, repos clone/sync | Git sin token en URL/argv; checkout limpio, fast-forward y timeout |
| repositorios locales | repos list/add/edit/remove | Validación del mapa; eliminación conserva archivos y protege dependencias |
| fuentes, descriptores y permisos | sources list/show/add/edit/remove/enable/disable/audience | Alta deshabilitada sin audiencias; allowlists de archivos/URLs/herramientas |
| cloudflared y configuración del túnel | tunnel status/configure/validate | Token solo en entorno hijo; hostname, origen y regla final validados |
| chat local | chat, test simulate | Pipeline compartido; adaptador de simulación nunca envía a Graph |
| habilitar/deshabilitar/probar chat personal | self-chat status/enable/disable/reconcile, test self-chat | Cuenta y membresía, o scope delegado y autores para notas; corte temporal, outbox y deduplicación durables |
| pruebas instaladas | doctor --offline, test providers/connectivity | Proveedores con evidencia ficticia; lectura Graph separada del envío |
| auditoría y diagnóstico | audit list/show, logs | Conexiones de lectura; contenido opt-in, errores de dependencias saneados |
| skill y acceso terminal | skill show/path/install, app install-cli | Textos de la versión incorporados; destino nuevo; ambos binarios en el bundle |

`config apply` expone el objeto Config completo: server, graph (incluidos chats/canales y self_chat), jev, llm, policy, secrets y knowledge_map. KnowledgeMap expone repositories y resources, con cada variante File, Url o Tool y sus campos. `config set` modifica un campo existente por ruta de puntos; objetos y listas se reemplazan como valores JSON. El mapa administrado permanece junto al perfil; cambiar data_dir requiere importación explícita mientras está detenido. Las reglas del escritorio sobre scopes de canales se conservan.

Las pruebas de integración cubren Graph/Jev/DeepSeek, observación, relectura antes de enviar, error de envío sin reintento, mensajes propios a terceros, historial previo, salida adelantada, restart y texto humano igual al de una respuesta. La regresión de notas prueba el endpoint delegado sin consultar metadata ChatThread y rechaza autores de otra cuenta. Las pruebas de control rechazan autenticación ausente/incorrecta y Origin de navegador.

La evidencia de instalación, operaciones reales y publicación de cada release se registra con los artefactos en REVIEW.md. La firma ad hoc no acredita notarización. Windows permanece en roadmap para la siguiente versión; su check de compilación no acredita funcionamiento.
