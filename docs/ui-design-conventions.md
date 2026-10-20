# GUI design conventions

The standard every change to `desktop/ui/` follows, so the native window, the web portal and the phone layout stay one consistent work tool. The architecture (§11) says what each section does; this document says how it looks and behaves. The reasoning behind the current structure is in the [UX redesign report](ux-redesign-report.md); the visual language (0.7.25) is a sticker sheet: playful neobrutalism kept at the density of a work tool.

## 1. Character

- A professional tool used every day, with a playful look: clarity, density and speed come first, and the play lives in color, outline and type, never in extra content. No marketing copy, decorative illustrations, gradients or entrance animations.
- One visual identity: warm paper (`--bg`), an ink outline (`--line`, 2px) around every surface and control, rounded display type (`--display`: SF Pro Rounded on Apple devices, a system face elsewhere) for headings, buttons, tabs and numbers, and system text to read. Each section has its sticker color (`--c-overview`, `--c-messages`, `--c-registration`, `--c-knowledge`, `--c-chat`, `--c-settings`) for its navigation tile, its page tile and its active tab on a phone; states have their own fills (§7).
- A hard offset shadow means «you can press this» or «this is lifted above the page»: buttons, pressable cards and outcome tiles (they sink into the shadow when pressed), an open message, the open task detail, the selected sidebar item, and what floats (toast, save bar, phone tab bar, dialogs). Panels, rows, tabs and filters are flat; never a blurred shadow.
- The only illustration is the assistant's face: the brand mark, Home's state (awake while it runs and answers, reading aside while it observes, looking up while something is missing, glancing around while it starts or stops, asleep when stopped) and the eyes of an empty list. It repeats what the text says and is hidden from assistive technology.
- Motion only answers an action or shows a live state (the face's blink and glance, the status dot's pulse, a spinner, a button sinking, a notice popping in). `prefers-reduced-motion` disables it.

## 2. Platform constraints

- Five code assets only: `index.html`, `style.css`, `app.js`, `i18n.js`, `transport.js`, plus the icons and the PWA manifest: `favicon.png` (browser tab), `apple-touch-icon.png` (180 px, iOS home screen, a full-bleed square cut just inside the tile so iOS's mask meets its rim), `icon-192.png`, `icon-512.png` (the tile with transparent corners) and `manifest.webmanifest` (all embedded by `gui.rs` and served by `web.rs` without Cloudflare Access: WebKit fetches a page's icons without cookies, so each is listed in `PUBLIC_ASSETS` and has an exact Access Bypass app from `scripts/configure-web-access.py`). They are rendered from `desktop/icons/source/app.png` (the glossy face tile on white, 1254 px, kept out of the crate) with `node scripts/render-icons.mjs`, which knows where the tile sits in that image and also writes the desktop app's icons (`desktop/icons/32x32.png`, `128x128.png`, `icon.ico`, also the tray icon) and the landing's. The sidebar's brand mark is `icon-192.png`; inside the interface the face is drawn flat (Home's state, empty lists). A new file needs both and their tests; a new icon also needs `PUBLIC_ASSETS` and the helper's list.
- The CSP forbids inline styles and scripts and `innerHTML`: build nodes with `el()`, set dynamic geometry through CSSOM properties (`node.style.width = …`), no `style=` attributes. No external fonts, images or scripts.
- The same code runs in WKWebView (macOS), WebView2/WebKitGTK and the portal's browsers: no engine-specific APIs. The tokens need `light-dark()` (Safari 17.5, Chromium 123, Firefox 120 or later). A clicked button is not focused in WebKit, so do not rely on `document.activeElement` after a click (pass the context explicitly, as `markDirty({ target })` does).
- Every operation goes through `invoke()` and the IPC contract; there are no GUI-only methods.

## 3. Language and copy

- The source text is English; `t()`/`tn()` with named `{placeholders}`, or static HTML. Every new string gets its `i18n.js` entry (`scripts/test-ui-i18n.cjs` fails otherwise). Keep trailing spaces in a translation when the source splits a sentence around inline elements (`<em>`, `<code>`).
- Sentence case everywhere. No all-caps labels or eyebrows.
- Name things by what the user does, not by how the system is built; a button says exactly what happens («Save changes», «Create task in Done»). Keep one verb per action across the flow (Save/Saved, Remove/Removed).
- Hints explain the consequence or the constraint in one or two sentences; a hint that only repeats the label is removed.
- Empty states say what is missing and how to get it; errors say what happened and how to fix it, without apologizing.

## 4. Information architecture

| Navigation | Contents |
| --- | --- |
| Daily work | Home (state, attention, setup, recent activity with its outcome tiles, status), To-Do, Messages, Activities |
| What feeds the answers | Knowledge (sources, repositories, GitHub), Test chat |
| Footer | Settings, then the status pill and the web account |

- A new feature joins the section of the task it serves; it does not get a navigation item of its own unless it is a separate daily task. The sidebar holds at most eight items. The phone keeps five destinations: Home, To-Do, Messages, Activities and More (Knowledge and Test chat). More is a keyboard-accessible disclosure, closes after selection or Escape, and preserves the floating bar's shape and touch targets.
- Settings is one category at a time: **General** (how the assistant answers, and this device's theme), **Microsoft Teams** (identity, reception, personal chat, public URL), **Language models**, **Activity registration** (daily record and schedule), **Credentials**, **Advanced** (rare or migration operations). A new field goes in the category of the user's goal; a category reaches seven panels before it is split, and a seventh category needs an IA review.
- Deep links use `showTab(section, panelId)`: when the panel is in Settings it opens its category first. Home's status rows and setup steps always link to where the thing is changed.
- Attention is shown once where it can be acted on (a running daily snapshot is shown on Home and at its save control).

## 5. Progressive disclosure

- Show what most users need for the task; fold the rest with `<details class="fold">` and a `small` «Advanced» note in the summary.
- A field that does not apply in the current mode is hidden, never cleared: its value is kept and still saved (allowed chats while discovering all chats; daily time vs interval and window).
- Prefer recognition over recall: choices are controls (the workday toggles), not syntaxes to remember («1, 2, 3»).
- Native date/time inputs use `appearance: none`, border-box sizing and a zero minimum width so iOS keeps them inside their labels; verify empty and populated controls at phone widths.
- Validate what the host would reject before sending, next to the field or in a notice that opens the right category (at least one workday).
- List rows summarize the item (state, description, location, audience) and open an editor fold; a fold remembers being open across re-renders.

## 6. Components

| Need | Use |
| --- | --- |
| Group of related settings or content | `.panel` with an `h2` (`.panel-head` when it has a badge or action) |
| Explanation under a title or field | `.hint`; `label > small` for a field's constraint |
| Fields side by side | `.grid` (stretches); `.grid.fill` when a short field may stand alone |
| Boolean with a consequence | `.switch-row` for the few mode switches; `.check` for ordinary options |
| Exclusive filter of a list | `.segmented` chips with `aria-pressed` and a `.count`; the pressed one is inverse (ink, cream in dark) |
| Category tabs inside a section | `.subnav` with `role=tablist`/`tab`/`tabpanel`, roving `tabindex`, arrow/Home/End keys (`selectTab(list, name)`), `.flag.edited` and `.flag.warn` dots with screen-reader text; pill tabs, the selected one inverse; it wraps, and below 860px it is a grid of large choices (Settings: icon over the name) |
| Status of a part, linking to its setting | `.status-row` button (icon tile in the tone's fill, label, value, detail, arrow) |
| Counts that open a filtered list | `.stat` tiles in a `.stat-grid` (number and label; the tone's soft fill only when the count is not zero) |
| State of an item | `badge(text, tone)`: `ok`, `warn`, `error`, `info`, `active`, `muted`; the text always carries the meaning, the color only reinforces it |
| Result or problem where it happened | `.callout` with a tone; global confirmations go to `notify()` (status) and failures to `fail()` (alert) |
| Selectable item in a list | `button.card` / `.card` |
| Unsaved form state | the save bar plus the edited section's flag; `markDirty(event)` records the section |

- Buttons: outlined pills with a shadow. One primary action per panel or form (the next step, `--primary` lime); everything else `.secondary` (paper); destructive actions `.danger` and visually apart; navigation-like actions `.link-button`. An irreversible action (dismiss, send to Teams) asks for a second tap instead of a modal.
- Asynchronous actions run through `busy(button, task)`: the button shows progress and is disabled; errors become a notice.
- To-Do cards open an in-section detail; Activities reuses these cards and detail after selecting a recorded day; the external-link icon remains a separate action. Back
  restores filters, list position and focus. Keep assignment, type-specific content and discussion
  visible; fold all fields and history. Parse provider HTML in the core into safe rich-text trees, rebuild from allowed elements and turn
  images into original links; never mount provider scripts, styles or image URLs.
- Links to Azure DevOps, Wiki and Teams artifacts use `link()` (opens in the portal, copies in the native window). To-Do link icons explicitly open the item: a new tab in the portal, the default browser through the validated `open_work_item` host operation in the native window.

## 7. Tokens

- Colors are semantic custom properties in `:root`, each holding its light and dark value with `light-dark()`; components never use raw hex values. `color-scheme` picks the theme: the system's, unless Settings › General › Appearance chose Light or Dark on this device (`data-theme` on the root, `pta-theme` in `localStorage`, applied before the first paint; not part of the profile). Dark mode keeps the same stickers on a deep violet-black, with cream ink.
- Every state has three tokens: a text color (`--ok`, `--warn`, `--error`, `--info`, `--active`), a sticker fill (`--*-fill`) and a soft fill (`--*-soft`). Text on a section color or a sticker fill is `--on-color` (ink) in both themes; text on a soft fill is `--text`. The inverse pair (`--inverse`, `--inverse-text`) marks what is selected.
- Text contrast is at least 4.5:1 on every surface and fill in both themes, `--muted` and `--faint` included (they carry small text). `--line` outlines every surface and control (WCAG 1.4.11, far above 3:1); `--rule` only separates rows inside a panel. Check new pairs before adding them.
- Disabled controls fade to 45% and lose their shadow.
- Border `--bw` (2px; badges 1.5px). Radius `--radius` (22px) for panels, `--radius-card` (16px) for cards, messages, callouts and the toast, `--radius-control` (14px) for inputs, 99px for buttons, chips, tabs, badges and pills.
- Shadows: `--shadow` (3px 3px 0) for what can be pressed, `--shadow-float` (5px 5px 0) for what floats or is open.
- Spacing in rem steps (.25, .5, .75, 1, 1.25); panels pad 1.15rem 1.3rem and separate by 1rem.
- Type: body 15px (16px on phones), `h1` 1.85rem weight 800 (1.6rem on phones), `h2` 1.12rem, hints .92rem, metadata .82–.86rem; tabular figures for counts and times.

## 8. Accessibility (WCAG 2.2 AA)

- Every control has a visible label (or an `aria-label` when it is an icon); placeholders never replace labels.
- Keyboard: a skip link to `main`; changing section focuses its `h1` (no visible ring after a click); visible `:focus-visible` rings; tab lists follow the ARIA pattern.
- Live regions: errors use `role=alert`, confirmations and progress `role=status`/`aria-live=polite`, without moving focus.
- Targets: at least 24px on desktop and 44px on phones (inputs, buttons — small and segmented ones too —, checks, tabs, workday toggles, icon buttons); the phone switch is native-sized.
- Hover styles live under `@media (hover: hover)`: on a touch screen a tap would leave them stuck. A pressed button dims instead.
- Never color alone: badges and dots come with text.

## 9. Responsive

- Above 860px: sidebar. 681–860px: a top bar with short labels and an icon-only Settings. 680px and below (iPhone 15: 393px): a title bar with the state and Settings, a floating tab bar of five sections (the active one shows its short name on its sticker color, the others their icon, named for assistive technology), safe-area insets, 16px inputs and 44px controls.
- No horizontal page scroll at any width, and the page does not bounce past its edges (`overflow-x: clip`, `overscroll-behavior: none` on the root): a phone user can never drag the whole screen. Tab lists wrap instead of scrolling sideways; only content that needs it (code blocks, wide tables) scrolls inside its own box.
- Lists switch from aligned columns to wrapping lines of facts (Messages summaries, status rows).

## 10. Verification

Run the checks of `AGENTS.md` (`bun build`, `node --test scripts/test-activity-ui.cjs scripts/test-ui-i18n.cjs`). With Playwright available, also look at the result and scan it:

```sh
npm install --prefix /tmp/pta-ui-preview playwright-core axe-core   # once, outside the repository
node scripts/ui-preview/server.mjs "" 4173 &                         # current desktop/ui with synthetic data
node scripts/ui-preview/check.mjs 4173 /tmp/ui-check                 # axe WCAG 2.2 AA + interaction checks
node scripts/ui-preview/shoot.mjs 4173 /tmp/ui-shots                 # screenshots: light, dark, fresh, phone, tablet
```

`check.mjs` fails on any failed interaction check or axe violation; CI runs it on every change to the GUI or its scripts. `PTA_UI_CHROMIUM` points at a Chromium executable when Playwright's own browsers are not installed. To compare with a previous version, export it (`git show REV:desktop/ui/FILE`) into a directory, serve it on another port and run `check.mjs PORT OUT --axe-only`. The mock (`scripts/ui-preview/mock.js`) only knows the IPC methods the screens read and the safe writes; extend it when a screen needs a new one, with synthetic data only.
