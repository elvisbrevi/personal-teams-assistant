// Settings › General and Microsoft Teams: chat reading and the Teams message style.
import { t } from './i18n.js';
import { $, field } from './dom.js';
import { current } from './state.js';

// ---------------------------------------------------------------- settings

/** The allowed-chats list only applies while not every chat is discovered; its value is kept either way. */
function renderChatList() {
  $('#allowed-chats-field').hidden = $('#discover-chats').checked;
  const all = $('#reading-all-chats').checked;
  $('#reading-personal-chat-field').hidden = all;
  $('#reading-chats-field').hidden = all;
}

// ---------------------------------------------------------------- Teams message style

/** Color combinations for the Teams card: any stripe color, and Teams' theme-aware band and text colors. */
const STYLE_PRESETS = {
  teams: { accent: '#5B5FC7', band: 'accent', label_color: 'default' },
  blue: { accent: '#2F80ED', band: 'accent', label_color: 'accent' },
  green: { accent: '#13A10E', band: 'good', label_color: 'good' },
  amber: { accent: '#CA5010', band: 'warning', label_color: 'warning' },
  red: { accent: '#E74856', band: 'attention', label_color: 'attention' },
  gray: { accent: '#8A8A8A', band: 'emphasis', label_color: 'subtle' },
};
/** `[message_style]` as the host defaults it; the host omits the section while it is unchanged. */
const STYLE_DEFAULT = { enabled: false, label: '', position: 'bottom', ...STYLE_PRESETS.teams, icon: true, stripe_width: 4, text_size: 'default', label_size: 'small' };

function renderMessageStyle() {
  const style = { ...STYLE_DEFAULT, ...current.config.message_style };
  $('#style-enabled').checked = style.enabled;
  field('#style-label', style.label);
  field('#style-position', style.position);
  field('#style-accent', style.accent.toLowerCase());
  field('#style-band', style.band);
  field('#style-label-color', style.label_color);
  $('#style-icon').checked = style.icon;
  field('#style-stripe-width', style.stripe_width);
  field('#style-text-size', style.text_size);
  field('#style-label-size', style.label_size);
  renderStylePreview();
}

/** The combination the color fields match, or custom. */
function stylePreset() {
  const accent = $('#style-accent').value.toUpperCase();
  const match = Object.entries(STYLE_PRESETS).find(([, preset]) =>
    preset.accent === accent && preset.band === $('#style-band').value && preset.label_color === $('#style-label-color').value);
  return match?.[0] ?? 'custom';
}

function renderStylePreview() {
  $('#style-fields').hidden = !$('#style-enabled').checked;
  $('#style-preset').value = stylePreset();
  const preview = $('#style-preview');
  preview.className = `teams-card band-${$('#style-band').value} label-${$('#style-label-color').value} at-${$('#style-position').value} text-${$('#style-text-size').value} label-size-${$('#style-label-size').value}`;
  preview.style.setProperty('--stripe', $('#style-accent').value);
  preview.style.setProperty('--stripe-width', `${stripeWidth() ?? 4}px`);
  // The default band text follows the reply language, like what the assistant sends.
  const fallback = $('#llm-language').value === 'en' ? 'AI-generated response' : 'Respuesta generada con IA';
  $('#style-preview-label').textContent = $('#style-label').value.trim() || fallback;
  $('#style-preview-icon').hidden = !$('#style-icon').checked;
}

/** The stripe width typed, when it is a whole number of pixels the host accepts. */
function stripeWidth() {
  const width = Number($('#style-stripe-width').value);
  return Number.isInteger(width) && width >= 2 && width <= 16 ? width : null;
}

function collectMessageStyle() {
  const stripe_width = stripeWidth();
  if (stripe_width === null) throw t('The stripe width is a whole number of pixels from 2 to 16.');
  current.config.message_style = {
    enabled: $('#style-enabled').checked,
    label: $('#style-label').value.trim(),
    position: $('#style-position').value,
    accent: $('#style-accent').value.toUpperCase(),
    band: $('#style-band').value,
    label_color: $('#style-label-color').value,
    icon: $('#style-icon').checked,
    stripe_width,
    text_size: $('#style-text-size').value,
    label_size: $('#style-label-size').value,
  };
}

export { renderChatList, STYLE_PRESETS, renderMessageStyle, renderStylePreview, collectMessageStyle };
