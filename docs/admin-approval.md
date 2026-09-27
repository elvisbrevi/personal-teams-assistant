# Solicitud de consentimiento Entra

La autorización delegada puede detenerse con `AADSTS90095: Admin consent is required`. Registrar una app y crear su secreto no concede acceso a chats.

Texto propuesto para revisión del administrador:

> Solicito autorizar una aplicación single-tenant para un asistente personal de Teams del usuario solicitante. Utiliza permisos delegados User.Read, Chat.Read, ChatMessage.Send y offline_access; no solicita permisos de aplicación ni acceso global a todos los usuarios. Opera desde un equipo local mediante HTTPS, inicialmente en modo dry-run sin enviar mensajes. Los saludos usan reglas locales. Las preguntas pueden enviarse redactadas a TypeSafe/Jev para decisiones y a DeepSeek para generación, junto con fragmentos de fuentes explícitamente autorizadas por conversación. No se envían credenciales a los modelos. Hay controles de confianza, redacción, auditoría y silencio ante información insuficiente. Se solicita revisión y consentimiento conforme a las políticas de seguridad y tratamiento de datos de la organización antes de su uso operativo.

Después del consentimiento, volver a iniciar OAuth desde `/oauth/login`; los estados y códigos anteriores caducan. Si la organización no permite este uso/procesamiento externo, no habilitarlo ni intentar evadir la política mediante otra identidad.
