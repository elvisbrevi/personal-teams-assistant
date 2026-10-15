# GUI design conventions

The standard every change to `desktop/ui/` follows, so the native window, the web portal and the phone layout stay one consistent work tool. The architecture (§11) says what each section does; this document says how it looks and behaves. The reasoning behind the current structure is in the [UX redesign report](ux-redesign-report.md).

## 1. Character

- A professional tool used every day: clarity, density and speed over decoration. No hero sections, marketing copy, illustrations, gradients or entrance animations.
- One visual identity: the dark green brand (`--brand`, the logo), system fonts, flat panels separated by borders. Elevation (`--shadow-float`) is only for what floats: the toast, the save bar and dialogs.
- Motion only answers an action or shows a live state (the running dot's pulse, a spinner). `prefers-reduced-motion` disables it.

## 2. Platform constraints

- Five assets only: `index.html`, `style.css`, `app.js`, `i18n.js`, `transport.js` (embedded by `gui.rs` and served by `web.rs`). A new file needs both and their tests.
- The CSP forbids inline styles and scripts and `innerHTML`: build nodes with `el()`, set dynamic geometry through CSSOM properties (`node.style.width = …`), no `style=` attributes. No external fonts, images or scripts.
- The same code runs in WKWebView (macOS), WebView2/WebKitGTK and the portal's browsers: no engine-specific APIs. A clicked button is not focused in WebKit, so do not rely on `document.activeElement` after a click (pass the context explicitly, as `markDirty({ target })` does).
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
| Daily work | Home (state, attention, setup, status, recent activity), Messages, Activities |
| What feeds the answers | Knowledge (sources, repositories, GitHub), Test chat |
| Footer | Settings, then the status pill and the web account |

- A new feature joins the section of the task it serves; it does not get a navigation item of its own unless it is a separate daily task. The sidebar holds at most eight items; the phone tab bar exactly the five non-Settings sections.
- Settings is one category at a time: **General** (how the assistant answers), **Microsoft Teams** (identity, reception, personal chat, public URL), **Language models**, **Activity registration**, **Credentials**, **Advanced** (rare or migration operations). A new field goes in the category of the user's goal; a category reaches seven panels before it is split, and a seventh category needs an IA review.
- Deep links use `showTab(section, panelId)`: when the panel is in Settings it opens its category first. Home's status rows and setup steps always link to where the thing is changed.
- Attention is shown once where it can be acted on (the activities notice is on Home; elsewhere the navigation counter carries it).

## 5. Progressive disclosure

- Show what most users need for the task; fold the rest with `<details class="fold">` and a `small` «Advanced» note in the summary.
- A field that does not apply in the current mode is hidden, never cleared: its value is kept and still saved (allowed chats while discovering all chats; daily time vs interval and window).
- Prefer recognition over recall: choices are controls (the workday toggles), not syntaxes to remember («1, 2, 3»).
- Validate what the host would reject before sending, next to the field or in a notice that opens the right category (at least one workday).
- List rows summarize the item (state, description, location, audience) and open an editor fold; a fold remembers being open across re-renders.

## 6. Components

| Need | Use |
| --- | --- |
| Group of related settings or content | `.panel` with an `h2` (`.panel-head` when it has a badge or action) |
| Explanation under a title or field | `.hint`; `label > small` for a field's constraint |
| Fields side by side | `.grid` (stretches); `.grid.fill` when a short field may stand alone |
| Boolean with a consequence | `.switch-row` for the few mode switches; `.check` for ordinary options |
| Exclusive filter of a list | `.segmented` buttons with `aria-pressed` and a `.count` |
| Category tabs inside a section | `.subnav` with `role=tablist`/`tab`/`tabpanel`, roving `tabindex`, arrow/Home/End keys, `.flag.edited` and `.flag.warn` dots with screen-reader text |
| Status of a part, linking to its setting | `.status-row` button (label, dot and value, detail, arrow) |
| State of an item | `badge(text, tone)`: `ok`, `warn`, `error`, `info`, `active`, `muted`; the text always carries the meaning, the color only reinforces it |
| Result or problem where it happened | `.callout` with a tone; global confirmations go to `notify()` (status) and failures to `fail()` (alert) |
| Selectable item in a list | `button.card` / `.card` |
| Unsaved form state | the save bar plus the edited section's flag; `markDirty(event)` records the section |

- Buttons: one primary action per panel or form (the next step); everything else `.secondary`; destructive actions `.danger` and visually apart; navigation-like actions `.link-button`. An irreversible action (dismiss, send to Teams) asks for a second tap instead of a modal.
- Asynchronous actions run through `busy(button, task)`: the button shows progress and is disabled; errors become a notice.
- Links to Azure DevOps, Wiki and Teams artifacts use `link()` (opens in the portal, copies in the native window).

## 7. Tokens

- Colors are semantic custom properties in `:root` with a dark variant under `prefers-color-scheme: dark`; components never use raw hex values except the save bar's on-brand white.
- Text contrast is at least 4.5:1 on every surface in both themes, `--faint` included (it carries small text). Check new pairs before adding them.
- Radius `--radius` (10px) for panels; 8px for controls; 99px only for badges and pills.
- Spacing in rem steps (.25, .5, .75, 1, 1.25); panels pad 1.05rem 1.2rem and separate by .9rem.
- Type: body 15px (16px on phones), `h1` 1.55rem, `h2` 1.05rem, hints .92rem, metadata .82–.86rem; tabular figures for counts and times.

## 8. Accessibility (WCAG 2.2 AA)

- Every control has a visible label (or an `aria-label` when it is an icon); placeholders never replace labels.
- Keyboard: a skip link to `main`; changing section focuses its `h1` (no visible ring after a click); visible `:focus-visible` rings; tab lists follow the ARIA pattern.
- Live regions: errors use `role=alert`, confirmations and progress `role=status`/`aria-live=polite`, without moving focus.
- Targets: at least 24px on desktop and 44px on phones (inputs, buttons, checks).
- Never color alone: badges and dots come with text.

## 9. Responsive

- Above 860px: sidebar. 681–860px: a top bar with short labels and an icon-only Settings. 680px and below (iPhone 15: 393px): a title bar with the state and Settings, a bottom tab bar of five sections, safe-area insets, 16px inputs and 44px controls.
- No horizontal page scroll at any width; only tab lists scroll sideways, and they keep the selected tab in view.
- Lists switch from aligned columns to wrapping lines of facts (Messages summaries, status rows).

## 10. Verification

Run the checks of `AGENTS.md` (`bun build`, `node --test scripts/test-activity-ui.cjs scripts/test-ui-i18n.cjs`). With Playwright available, also look at the result and scan it:

```sh
npm install --prefix /tmp/pta-ui-preview playwright-core axe-core   # once, outside the repository
node scripts/ui-preview/server.mjs "" 4173 &                         # current desktop/ui with synthetic data
node scripts/ui-preview/check.mjs 4173 /tmp/ui-check                 # axe WCAG 2.2 AA + interaction checks
node scripts/ui-preview/shoot.mjs 4173 /tmp/ui-shots                 # screenshots: light, dark, fresh, phone, tablet
```

`PTA_UI_CHROMIUM` points at a Chromium executable when Playwright's own browsers are not installed. To compare with a previous version, export it (`git show REV:desktop/ui/FILE`) into a directory, serve it on another port and run `check.mjs PORT OUT --axe-only`. The mock (`scripts/ui-preview/mock.js`) only knows the IPC methods the screens read and the safe writes; extend it when a screen needs a new one, with synthetic data only.
