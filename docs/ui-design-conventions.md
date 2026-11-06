# GUI design conventions

The standard every change to `desktop/ui/` follows, so the owner's local page, the web portal and the phone layout stay one consistent work tool. The architecture (§11) says what each section does; this document says how it looks and behaves. The default visual language is a sticker sheet: playful neobrutalism kept at the density of a work tool. Each device can choose another style and palette (§7); every rule here holds in all of them, and what only one style does is named.

## 1. Character

- A professional tool used every day, with a playful look: clarity, density and speed come first, and the play lives in color, outline and type, never in extra content. No marketing copy, decorative illustrations, gradients or entrance animations.
- Three styles draw the same components (Settings › General › Appearance): **Stickers** (the default, described in this section), **Soft** (recommended for long sessions: fine lines, soft blurred shadows under surfaces and floating parts, rounded rectangles, a friendlier display face, the section colors as tints) and **Minimalist** (flat and compact: fine lines, small corners, no section tiles, figures in monospace, ink as the only accent). A style changes tokens first (§7) and adds rules only for what a token cannot say; a component never tests the style itself.
- Stickers' identity: warm paper (`--bg`), an ink outline (`--line`, 2px) around every surface and control, rounded display type (`--display`: SF Pro Rounded on Apple devices, a system face elsewhere) for headings, buttons, tabs and numbers, and system text to read. Each section has its sticker color (`--c-overview`, `--c-messages`, `--c-registration`, `--c-knowledge`, `--c-chat`, `--c-settings`) for its navigation tile, its page tile and its active tab on a phone; states have their own fills (§7).
- In Stickers a hard offset shadow means «you can press this» or «this is lifted above the page»: buttons, pressable cards and outcome tiles (they sink into the shadow when pressed), an open message, the open task detail, the selected sidebar item, and what floats (toast, save bar, phone tab bar, dialogs). Panels, rows, tabs and filters are flat; never a blurred shadow. Soft and Minimalist have no hard shadow: a pressed button takes a wash (`--wash`) instead of sinking.
- The only illustration is the assistant's face: the brand mark, Home's state (awake while it runs and answers, reading aside while it observes, looking up while something is missing, glancing around while it starts or stops, asleep when stopped) and the eyes of an empty list. It repeats what the text says and is hidden from assistive technology.
- Motion only answers an action or shows a live state (the face's blink and glance, the status dot's pulse, a spinner, a button sinking, a notice popping in). `prefers-reduced-motion` disables it.

## 2. Platform constraints

- Five code assets only: `index.html`, `style.css`, `app.js`, `i18n.js`, `transport.js`, plus the icons and the PWA manifest: `favicon.png` (browser tab), `apple-touch-icon.png` (180 px, iOS home screen, a full-bleed square cut just inside the tile so iOS's mask meets its rim), `icon-192.png`, `icon-512.png` (the tile with transparent corners) and `manifest.webmanifest` (all embedded by `page.rs` and `web.rs`; the portal serves them without Cloudflare Access: WebKit fetches a page's icons without cookies, so each is listed in `PUBLIC_ASSETS` and has an exact Access Bypass app from `scripts/configure-web-access.py`). They are rendered from `desktop/icons/source/app.png` (the glossy face tile on white, 1254 px, kept out of the crate) with `node scripts/render-icons.mjs`, which knows where the tile sits in that image and also writes the landing's. The sidebar's brand mark is `icon-192.png`; inside the interface the face is drawn flat (Home's state, empty lists). A new file needs both and their tests; a new icon also needs `PUBLIC_ASSETS` and the helper's list.
- The CSP forbids inline styles and scripts and `innerHTML`: build nodes with `el()`, set dynamic geometry through CSSOM properties (`node.style.width = …`), no `style=` attributes. No external fonts, images or scripts.
- The same code runs in every current browser, on the local page and in the portal (Safari, Chromium-based browsers and Firefox): no engine-specific APIs. The tokens need `light-dark()` (Safari 17.5, Chromium 123, Firefox 120 or later). A clicked button is not focused in WebKit, so do not rely on `document.activeElement` after a click (pass the context explicitly, as `markDirty({ target })` does).
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
| Daily work | Home (state, attention, setup, recent activity with its outcome tiles, status), To-Do (and its views: Create request, Implement, Agent history, Guides and instructions), Messages, Activities |
| What feeds the answers | Knowledge (sources, repositories, GitHub), Test chat |
| Footer | Settings, then the status pill and the web account |

- A new feature joins the section of the task it serves; it does not get a navigation item of its own unless it is a separate daily task. The sidebar holds at most eight items. A view that belongs to a section opens from it, has a Back button and keeps that section's navigation item current. The phone keeps five destinations: Home, To-Do, Messages, Activities and More (Knowledge and Test chat). More is a keyboard-accessible disclosure, closes after selection or Escape, and preserves the floating bar's shape and touch targets.
- Settings is one category at a time: **General** (how the assistant answers, and this device's appearance: style, palette and mode), **Microsoft Teams** (identity, reception, personal chat, public URL), **Models**, **Machines** (this computer and the account's machines, in the one list that also chooses which answers Teams), **Daily record** (daily record and schedule), **Variables** (application credentials and other stored variables in one list), **Advanced** (rare or migration operations). A new field goes in the category of the user's goal; a category reaches seven panels before it is split, and an eighth category needs an IA review. Category names stay short enough for the seven tabs to share one row on a desktop.
- Deep links use `showTab(section, panelId)`: when the panel is in Settings it opens its category first. Home's status rows and setup steps always link to where the thing is changed.
- Attention is shown once where it can be acted on (a running daily snapshot is shown on Home and at its save control).

## 5. Progressive disclosure

- Show what most users need for the task; fold the rest with `<details class="fold">` and a `small` «Advanced» note in the summary.
- Show the next step and fold what maintains: in a panel with many actions (Cloud storage) the actions that move the flow forward stay in sight and the rest go under a «Maintenance» fold, hidden while it has nothing to offer.
- A panel that is set up once folds while it is configured (`details.panel.fold-panel` with what it holds beside its title: Public URL and tunnel). It opens by itself while it still needs setup and whenever a link leads to it (`showTab` opens a `details` target).
- A status list shows what needs attention; what is in order folds into one line that opens it (Home's status).
- A field that does not apply in the current mode is hidden, never cleared: its value is kept and still saved (allowed chats while discovering all chats; daily time vs interval and window).
- Prefer recognition over recall: choices are controls (the workday toggles), not syntaxes to remember («1, 2, 3»).
- All directory inputs, selectors and hierarchy browsers use `chooseDirectories()`: a modal with compact full-path rows, folder icons, selection marks, parent navigation and an explicit confirmation footer. A `../` row comes first whenever a directory is open, in addition to the header's Back button; it never escapes the authorized roots. On phones it is a bottom sheet above the navigation, with its own scrolling list; it follows the app's current theme. Selections survive directory navigation, Cancel/Escape discards edits and closing restores trigger focus. Use single selection for a repository path and multiple selection for working directories or allowed roots. Do not use free-text directory inputs, newline-separated directory textareas or inline tree panels. This applies to every machine, the local checkout add form and existing checkout editors. File paths (configuration imports, tunnel configuration, relative source files) remain file fields.
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

- Buttons: outlined pills with a shadow in Stickers, rounded rectangles in the other styles. One primary action per panel or form (the next step, `--primary`: the palette's first section color, or ink in Minimalist, with `--on-primary` text); everything else `.secondary` (paper); destructive actions `.danger` and visually apart; navigation-like actions `.link-button`. An irreversible action (dismiss, send to Teams) asks for a second tap instead of a modal: the first tap changes the label to «Tap again to…» for about four seconds, and the account store's actions (Settings › Advanced › Cloud storage), which back up, upload or reseal the whole account, stay armed twelve seconds, each re-arm restarting the whole window.
- Asynchronous actions run through `busy(button, task)`: the button shows progress and is disabled; errors become a notice.
- Confidential values (stored variables): lists and details never carry them. A value appears only after an explicit Reveal or Copy value, fetched for that one entry; it lives in memory and the element that shows it (never `localStorage`, a URL or history) and is cleared on Hide, navigation, a hidden page, logout and after 30 seconds. Copy writes the exact value without echoing it in a notice. Value fields are masked textareas with an explicit «Show the value while I type» choice, and a button's `aria-label` must contain its visible text.
- Home includes the Azure CLI and GitHub CLI sessions in its status list: installed/authenticated state and the current account, with a direct refresh action. Missing authentication explains `az login` or `gh auth login` on the machine running the app; it never asks for a write token.
- To-Do places Create request below filters, with context, searchable project/repository sheets and the shared CLI/model/effort controls. Each sheet starts with a name filter, then up to three recent destinations and all remaining destinations without duplicates; filtering matches names regardless of case or accents. Escape/Cancel preserves the choice and restores trigger focus; phones use the existing bottom-sheet presentation. Azure offers HU/bug radios; GitHub fixes the type to Epic. Agent history shows app run IDs separately from native session IDs and reuses the existing session opener. The list starts with ten items and «Show more» asks for the next step (25, 50, 100), keeping what is shown and moving focus to the first new item. A card shows the two recorded dates nearest to today (when it was created if none is recorded); the detail lists every date.
- To-Do cards open an in-section detail; Activities reuses these cards and detail after selecting a recorded day; the external-link icon remains a separate action. Back
  restores filters, list position and focus. Keep assignment, type-specific content and discussion
  visible; fold all fields and history. Parse provider HTML in the core into safe rich-text trees, rebuild from allowed elements and turn
  images into original links; never mount provider scripts, styles or image URLs.
- Links to Azure DevOps, Wiki and Teams artifacts use `link()` (opens in the portal, copies on the local page). To-Do link icons explicitly open the item: a new tab in the portal, the default browser through the validated `open_work_item` host operation on the local page.

## 7. Tokens

- Colors are semantic custom properties, each holding its light and dark value with `light-dark()`; components never use raw hex values. Three choices of this device shape the look, each a button group in Settings › General › Appearance, kept in `localStorage` (not part of the profile) and applied before the first paint; a missing or unknown value is the default:
  - **Mode** (`data-theme`, `pta-theme`): `color-scheme` follows the system unless Light or Dark was chosen.
  - **Palette** (`data-palette`, `pta-palette`): Paper (the default: warm paper, and a deep violet-black with cream ink in dark mode), Catppuccin, Monokai, Dracula, Nord, Solarized, Gruvbox, Tokyo Night, Rosé Pine and Graphite, each with a light and a dark side. A palette gives the surfaces, the text, the link, `--on-color` and one fill per hue; `[data-palette]:not([data-palette="paper"])` derives the rest (state text, soft fills, rule, inverse, bar) so every palette follows the same contrast rules. Well-known palettes keep their character, with accents lightened where ink must sit on them.
  - **Style** (`data-style`, `pta-style`): Stickers, Soft or Minimalist. A style sets the structural tokens: `--line`, `--edge`, `--bw`, the radii, the shadows (`--shadow`, `--shadow-float`, `--rest-shadow`, `--raised-shadow`, `--press`), `--fill-strength` with `--on-fill`, `--primary` with `--on-primary`, and its type (below).
- A palette or style block also matches any element that carries its attribute, so each choice in Appearance previews itself with its own tokens. A new palette is one block of base tokens plus its button; a new token pair is added to `scripts/test-ui-themes.cjs`, which resolves `var()`, `light-dark()` and `color-mix(in srgb)` from `style.css` and checks every pair for each palette, mode and style.
- Every state has three tokens: a text color (`--ok`, `--warn`, `--error`, `--info`, `--active`), a sticker fill (`--*-fill`) and a soft fill (`--*-soft`). Text on a section color or a sticker fill is `--on-color` (ink) in both modes; text on a soft fill is `--text`. A tile, the hero or a banner takes its color at `--fill-strength` over the surface (100% in Stickers, a tint in the other styles) and carries `--on-fill`. The inverse pair (`--inverse`, `--inverse-text`) marks what is selected.
- Text contrast is at least 4.5:1 on every surface and fill in every palette and mode, `--muted` and `--faint` included (they carry small text). `--edge` bounds inputs, checkboxes and radios at 3:1 or more (WCAG 1.4.11) in every style; `--line` outlines surfaces and the other controls (ink in Stickers, a fine line elsewhere); `--rule` only separates rows inside a panel. `scripts/test-ui-themes.cjs` fails when a pair falls short.
- Disabled controls fade to 45% and lose their shadow.
- Border `--bw` (2px in Stickers, 1px elsewhere; badges 1.5px). Radius `--radius` for panels, `--radius-card` for cards, messages, callouts and the toast, `--radius-control` for inputs, `--radius-tile` for icon tiles, `--radius-button` for buttons, tabs and filters, `--radius-chip` for badges, pills and counts, `--radius-bar` for the phone's tab bar (Stickers: 22, 16, 14, 12 and 99px for the last three).
- Shadows: `--shadow` for what can be pressed or is lifted, `--shadow-float` for what floats or is open (Stickers: 3px 3px 0 and 5px 5px 0; Soft: blurred, from `--shade`; Minimalist: none, and a blurred one only under what floats).
- Spacing in rem steps (.25, .5, .75, 1, 1.25); panels pad 1.15rem 1.3rem and separate by 1rem.
- Type: body 15px (16px on phones; Minimalist uses 14px above 680px), `h1` 1.85rem (1.6rem on phones), `h2` 1.12rem, and one scale for everything smaller: `--fs-lg` (1.08rem, titles of parts), 1rem (body), `--fs-md` (.92rem: hints, labels, buttons), `--fs-sm` (.86rem: secondary text, a field's constraint) and `--fs-xs` (.8rem: metadata). A new rule takes one of these, never a size of its own; the Teams card preview keeps Teams' sizes. A paragraph of explanation (`p.hint`) is at most 82 characters wide. Tabular figures for counts and times.
- Each style has its type, all from system faces (no font files): `--display` for titles, buttons, tabs and names, `--sans` to read, `--mono` for code and identifiers, `--numeric` for figures, the weights `--w-strong`, `--w-heavy` and `--w-nav`, and the tracking of titles (`--track`, `--track-tight`). Stickers: rounded display (SF Pro Rounded on Apple devices), heavy (750/800). Soft: a geometric humanist display where the system has one (Avenir Next on Apple devices, Segoe UI Variable on Windows) at 600/700. Minimalist: the system face for everything at 600/650 with lighter navigation, and monospace for figures, dates and times.

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

Run the checks of `AGENTS.md` (`bun build`, `node --test scripts/test-activity-ui.cjs scripts/test-ui-i18n.cjs scripts/test-ui-themes.cjs`). With Playwright available, also look at the result and scan it:

```sh
npm install --prefix /tmp/pta-ui-preview playwright-core axe-core   # once, outside the repository
node scripts/ui-preview/server.mjs "" 4173 &                         # current desktop/ui as the local page, synthetic data
node scripts/ui-preview/check.mjs 4173 /tmp/ui-check                 # axe WCAG 2.2 AA + interaction checks
node scripts/ui-preview/shoot.mjs 4173 /tmp/ui-shots                 # screenshots: light, dark, fresh, phone, tablet
```

`check.mjs` fails on any failed interaction check or axe violation (it also scans every palette in light and dark, rotating the styles); CI runs it on every change to the GUI or its scripts. `PTA_UI_CHROMIUM` points at a Chromium executable when Playwright's own browsers are not installed. To compare with a previous version, export it (`git show REV:desktop/ui/FILE`) into a directory, serve it on another port and run `check.mjs PORT OUT --axe-only`. The server serves the GUI as the owner's local page (with the portal's CSP) over a mocked host behind its fetch transport, so no host, profile or credential is involved. The mock (`scripts/ui-preview/mock.js`) only knows the IPC methods the screens read and the safe writes; extend it when a screen needs a new one, with synthetic data only.

Stored variables use an eye icon with Reveal/Hide text and an accessible pressed state. Values stay hidden until an explicit action and clear on navigation, a hidden page and after 30 seconds. Existing application credentials migrate before use; there is no separate Credentials editor. Cross-account credential migration remains under Advanced.
