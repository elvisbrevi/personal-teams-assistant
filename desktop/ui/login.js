const form = document.querySelector('#login-form');
const button = document.querySelector('#login-submit');
const error = document.querySelector('#login-error');
form.onsubmit = async event => {
  event.preventDefault();
  button.disabled = true;
  error.textContent = '';
  try {
    const reply = await fetch('/api/login', {
      method: 'POST', credentials: 'same-origin', cache: 'no-store',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: form.username.value.trim(), password: form.password.value }),
    });
    form.password.value = '';
    if (reply.ok) { window.location.replace('/'); return; }
    error.textContent = reply.status === 429 ? 'Too many sign-in attempts. Try again in a minute.'
      : reply.status === 401 ? 'The username or password is incorrect.'
      : 'Sign-in could not complete. Check the service and try again.';
  } catch {
    error.textContent = 'Cannot reach the service. Check your connection and try again.';
  } finally { button.disabled = false; }
};
