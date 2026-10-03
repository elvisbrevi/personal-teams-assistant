// Copy buttons and the install tabs. The page works without JavaScript; this only adds conveniences.
async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const area = document.createElement('textarea');
    area.value = text;
    area.setAttribute('readonly', '');
    area.style.position = 'fixed';
    area.style.opacity = '0';
    document.body.append(area);
    area.select();
    let copied = false;
    try { copied = document.execCommand('copy'); } catch { copied = false; }
    area.remove();
    return copied;
  }
}

for (const button of document.querySelectorAll('.copy')) {
  button.addEventListener('click', async () => {
    const text = button.dataset.copy ?? button.closest('[data-copy]')?.dataset.copy;
    if (!text) return;
    const label = button.textContent;
    const copied = await copy(text);
    button.textContent = copied ? 'Copiado' : 'Selecciona y copia';
    button.classList.toggle('copied', copied);
    setTimeout(() => { button.textContent = label; button.classList.remove('copied'); }, 1800);
  });
}

const tabs = [...document.querySelectorAll('[role=tab]')];
function select(tab) {
  for (const other of tabs) {
    const selected = other === tab;
    other.setAttribute('aria-selected', String(selected));
    other.tabIndex = selected ? 0 : -1;
    document.getElementById(other.getAttribute('aria-controls')).hidden = !selected;
  }
}
tabs.forEach((tab, index) => {
  tab.addEventListener('click', () => select(tab));
  tab.addEventListener('keydown', event => {
    const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
    if (!step) return;
    const next = tabs[(index + step + tabs.length) % tabs.length];
    select(next);
    next.focus();
  });
});
// Without JavaScript both panels stay visible.
if (tabs.length) select(tabs[0]);
