---
name: guardar-conocimiento
description: Guarda o actualiza datos en la base de conocimiento de Jev cuando el usuario pide recordar, registrar, corregir u organizar información para futuras respuestas.
---

# Guardar conocimiento

La unidad de trabajo es una **fuente**: un documento temático que Jev pueda elegir por su descriptor y del que la app pueda extraer pasajes útiles. La app solo consulta archivos declarados en `knowledge-map.toml` al iniciar.

1. **Ubica la fuente.** Lee el mapa configurado por la app y el `README.md` del checkout indicado por `repositories.personal` (en la instalación local, `../personal-teams-knowledge`). Busca por tema, sinónimos e identificadores en el mapa y en ese repositorio. Decide si corresponde corregir una fuente existente o crear una nueva. Termina cuando hayas identificado el documento canónico y cualquier dato contradictorio que deba actualizarse.
2. **Fija el hecho.** Conserva exactamente lo que afirma el usuario y su procedencia. Usa la fecha efectiva indicada; si falta, registra la fecha de recepción como fecha de registro, sin convertirla en vigencia del hecho. Si hay versiones incompatibles, deja clara la versión vigente y la procedencia de la anterior. Termina cuando cada afirmación nueva tenga sujeto, alcance, estado temporal y procedencia comprensibles, o esté marcada explícitamente como dato pendiente de confirmar.
3. **Escribe el documento.** Aplica el formato del contrato. Mantén un tema estable por archivo y un hecho autocontenido por párrafo para que `excerpt()` pueda recuperar pasajes completos. Actualiza el archivo existente cuando trata el mismo hecho; crea `temas/<dominio>/<asunto>.md` cuando sea una fuente nueva. Termina cuando no haya duplicados activos ni afirmaciones añadidas por inferencia.
4. **Registra el acceso.** Para una fuente nueva, añade una entrada `kind = "file"` en el `knowledge-map.toml` local siguiendo el ejemplo del contrato, con `id` estable, descripción que diga qué preguntas responde, `topics` con términos que usaría quien pregunta, y la ruta relativa real. Conserva las autorizaciones existentes al editar una fuente. Para una fuente nueva, usa solo conversaciones y remitentes aprobados explícitamente para ese contenido; si faltan, conserva `external_processing = false` y `allowed_conversations = []`, e informa que la fuente aún no es consultable. Termina cuando el mapa apunta al archivo correcto y su audiencia coincide con la autorización recibida.
5. **Comprueba la carga.** Valida el TOML y ejecuta `cargo run -- check config.toml` desde la app cuando exista esa configuración. Comprueba que la ruta declarada existe, queda dentro del checkout Git, tiene extensión admitida y el archivo pesa menos de 1 MB. Termina cuando la configuración carga sin error y queda claro si la fuente es consultable por Jev. Informa el archivo, el `id`, la audiencia y que la app debe reiniciarse para cargar cambios del mapa o contenido.

El repositorio de conocimiento es privado. Guarda allí solo la información necesaria para responder; las credenciales van fuera de él. El mapa local está excluido de Git: evita copiar datos privados a `knowledge-map.example.toml`.
