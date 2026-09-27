# Microsoft Entra y Graph

## Registro

1. Registrar una aplicación **single tenant** en el tenant donde está la cuenta Teams de trabajo/escuela. Una cuenta Microsoft personal no cubre los permisos Teams requeridos.
2. En plataforma Web registrar `https://HOST/oauth/callback`. No marcar como SPA; el servidor intercambia el código con secreto y PKCE.
3. Añadir permisos **delegados** `User.Read`, `Chat.Read`, `ChatMessage.Send`, `offline_access`. No usar `Chat.Read.All`, `Chat.ReadWrite.All` ni permisos de aplicación para el MVP.
4. Crear un secreto con caducidad limitada y guardarlo en `ENTRA_CLIENT_SECRET` usando `lazy-workflow credentials-set`. Nunca copiarlo a un archivo del proyecto.
5. Obtener Directory (tenant) ID, Application (client) ID y Object ID del usuario. Completar `[graph]`.
6. Si se habilitan canales, añadir `ChannelMessage.Read.All` y `ChannelMessage.Send`, y consentir según las reglas del tenant. No se necesitan para chats.
7. Iniciar sesión desde `/oauth/login`. Microsoft puede exigir aprobación de un administrador aunque el permiso admita consentimiento de usuario; depende de la política corporativa.

El servicio comprueba `/me` antes de guardar tokens para impedir conectar una cuenta distinta. Los tokens se cifran con AES-256-GCM-SIV y se guardan en SQLite; la clave reside fuera del estado. El refresh se serializa y cada token rotado se persiste antes de usarse. Revocación, Conditional Access o expiración pueden exigir autorización interactiva de nuevo.

## Suscripciones

Se usa Graph `v1.0`, una suscripción `created` por `/chats/{id}/messages`. Para todos los chats, se descubre `/me/chats` cada minuto con paginación, incluyendo nuevos chats en la siguiente pasada. Los mensajes anteriores a crear la suscripción pueden quedar fuera de cobertura.

No se usa `/chats/getAllMessages`: ese recurso global requiere permisos de aplicación. El recurso de notificación por usuario aparece documentado con un ejemplo beta; el MVP evita depender de él.

Las suscripciones básicas (`includeResourceData=false`) no transportan contenido ni requieren certificados de cifrado de notificaciones. El webhook compara `clientState` en tiempo constante, verifica tenant, suscripción persistida, colección y sintaxis de la ruta; luego Graph devuelve el mensaje usando el token delegado. No se sigue ninguna URL proporcionada por la notificación.

Las suscripciones duran 50 minutos y se renuevan cuando faltan 10. Se manejan `reauthorizationRequired`, `subscriptionRemoved` y `missed`. La recuperación `missed` solo encola la página reciente y mantiene el filtro de antigüedad; no garantiza recuperar todo un apagón. Si no se puede verificar, el mensaje queda manual.

El envío es `POST /chats/{id}/messages` con `contentType=text`; Graph determina el remitente usando el token delegado. No se crea un bot de Teams ni se falsifica el campo `from`.

## Referencias oficiales consultadas

- [Notificaciones de mensajes Teams](https://learn.microsoft.com/en-us/graph/teams-changenotifications-chatmessage)
- [Permiso ChatMessage.Send](https://learn.microsoft.com/en-us/graph/permissions-reference#chatmessagesend)
- [Envío de mensajes](https://learn.microsoft.com/en-us/graph/api/chatmessage-post?view=graph-rest-1.0)
- [OAuth authorization code y PKCE](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-auth-code-flow)
- [Entrega de webhooks](https://learn.microsoft.com/en-us/graph/change-notifications-delivery-webhooks)
