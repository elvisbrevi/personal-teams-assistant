const session = crypto.randomUUID();
const form = document.querySelector('#chat');
const history = document.querySelector('#history');
const send = document.querySelector('#send');

function turn(className, label, text) {
  const item = document.createElement('div');
  item.className = `turn ${className}`;
  item.textContent = `${label}: ${text}`;
  history.append(item);
}

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  const key = document.querySelector('#key').value;
  const field = document.querySelector('#message');
  const text = field.value.trim();
  if (!key || !text) return;
  turn('you', 'Tú', text);
  field.value = '';
  send.disabled = true;
  try {
    const response = await fetch('/test/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${key}` },
      body: JSON.stringify({ session, text, group: document.querySelector('#group').checked, mentioned: document.querySelector('#mention').checked })
    });
    if (response.status === 401) throw new Error('Clave de administración incorrecta.');
    const result = await response.json();
    if (!response.ok) throw new Error('No se pudo consultar una fuente o proveedor.');
    if (result.answer) turn('assistant', 'Asistente', result.answer);
    else turn('quiet', 'Asistente', `Sin respuesta automática (${result.reason || result.status}).`);
  } catch (error) {
    turn('quiet', 'Error', error.message);
  } finally {
    send.disabled = false;
    field.focus();
  }
});
