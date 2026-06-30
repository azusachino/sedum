# Miku — Miku-flavored redesign · handoff for `azusachino/miku`

A full visual redesign of **Miku** (the `miku` crate; the repo is named `miku`), built to drop into its **actual** stack — Rust + axum serving **server-rendered HTML, classic readonly-view / `<textarea>`-edit, no JS bundler**. The look is "vocaloid energy": Hatsune Miku's signature **teal `#39C5BB` → pink `#FF6FB5`** gradient, glassy panels, soft animated background orbs, an audio-equalizer motif, and a polished **light + dark** toggle.

> **All Miku visuals here are original/abstract** (color, gradient, waveform bars). No copyrighted character art or logos — keep it that way.

---

## TL;DR — how to put this in your project

1. Copy `static/miku.css` → your repo's `static/miku.css`.
2. (Optional) Copy `static/miku.js` → `static/miku.js` for theme persistence + ⌘K/⌘N + create-page live-slug. Everything works without it.
3. Use the files in `templates/` as the markup contract for your axum handlers. They're plain HTML with `miku.css` classes; port the `{{ … }}` holes to your Rust templating (askama / minijinja / maud / plain `format!`). The **markup is the spec**, the template syntax is illustrative.
4. Self-host the three fonts (Quicksand, Nunito, JetBrains Mono) or keep the Google Fonts `<link>` shown in `_shell.html`.

Open `preview.html` in a browser to see the rendered result with the real CSS.

---

## Why this isn't "drop in the prototype"

The original mockup (`Miku Wiki.dc.html`, included for reference) is a **React-style single file** with client-side tab routing, modals, and keystroke-live preview. **That doesn't match your architecture**, and pretending it does would fight the repo's design decisions. From `docs/architecture.md` / `CLAUDE.md`:

- **Rendering model is classic, server-side, no client JS:** `GET /page/:name` (readonly, primary) · `GET /page/:name/edit` (`<textarea>`) · `POST /page/:name` (atomic save → 303 redirect). Markdown is rendered **server-side by comrak**.
- **Side-by-side live preview is roadmap**, explicitly pairing with **CodeMirror 6** — it's listed under *Out of scope* today.
- **MDX/JSX is rejected** (ADR-2 / `miku:decision:no-mdx`): needs a JS runtime, breaks plain-Markdown portability.
- **No JS bundler** in v0.

So the redesign is delivered the way Miku actually ships it: **one stylesheet + server-render templates + an optional tiny vanilla-JS enhancement**. Nearly the entire design is CSS, so fidelity is fully preserved; only the *interaction plumbing* is re-expressed as server routes instead of a SPA.

---

## What maps to what

| Design surface (prototype) | Miku route | Template here | Notes |
|---|---|---|---|
| App shell (topbar + sidebar) | every page | `templates/_shell.html` | wraps all views; holds theme/accent attrs |
| Reading view | `GET /page/:name` | `templates/page_view.html` | comrak output styled by `.sd-prose` |
| Edit mode | `GET /page/:name/edit` → `POST /page/:name` | `templates/page_edit.html` | textarea form; preview pane = **roadmap**, see below |
| Search | `GET /search?q=` | `templates/search.html` | backed by `tb_pages.body_tsv` FTS |
| Tags | `GET /tags`, `GET /tags/:tag` | `templates/tags.html` | tag cloud sized by `tb_tags` counts |
| New-page flow | `GET /new` → `POST /new` | `templates/new_page.html` | modal **or** plain page; both no-bundler |
| Onboarding empty state | `GET /page/:name` (empty body) | bottom of `new_page.html` | shown until the page has content |
| Settings / appearance | `GET /settings` or a modal | `_shell.html` controls + `.sd-modal` styles | theme/accent are client prefs (localStorage) |

### Edit view — the one real decision for you
The design shows a split **source + live preview**. True keystroke-live preview is roadmap (CodeMirror 6). Until you adopt CM6, pick one — both honor "no bundler", and `page_edit.html` is commented for both:
- **(a) Single pane.** Drop the preview pane; the textarea fills the width. Simplest; fully classic.
- **(b) Server preview.** A "Preview" submit re-renders via comrak and repaints the right pane on POST — a classic round-trip, no client JS.

Do **not** wire a client-side Markdown library to fake live preview — that reintroduces exactly what ADR-2 rejected.

---

## Theming model (matches the no-bundler constraint)

- **Mode** = `data-theme="dark|light"` on `<html>`. **Accent** = `data-accent="miku|ocean|sakura|mono"`. Switching either just swaps CSS variables — no reflow, no rebuild.
- `static/miku.js` persists both to `localStorage` (`miku:theme`, `miku:accent`) and reflects active state onto any `[data-set-theme]` / `[data-set-accent]` control. The `<head>` snippet in `_shell.html` restores them **before first paint** (no flash).
- If you'd rather keep prefs **server-side** (per-user, no JS at all), read a cookie in the handler and emit `data-theme`/`data-accent` on `<html>` directly — the CSS doesn't care where the attribute comes from.

All tokens (the exact dark/light values, the gradient, type scale, radii, shadows, spacing) live at the top of `miku.css` as CSS custom properties — that file is the source of truth; this README won't duplicate the hex codes.

---

## Rendering Miku's Markdown into the design

`page_view.html` drops comrak's HTML into `<div class="sd-prose">…</div>`. `miku.css` already styles headings, lists (custom gradient markers), code, **fenced code blocks**, blockquotes, links, and **comrak `[!NOTE]` alert callouts** (`.sd-callout`). Two selectors to confirm against your comrak config:

- **Wikilinks:** styled via `.sd-prose a.wikilink` (and `.is-dangling` → pink dashed underline for unresolved `[[links]]`, which your indexer already distinguishes via `tb_links.target_id IS NULL`). Adjust the selector to whatever class/attr comrak emits for wikilinks in your build.
- **Headings → TOC / anchors:** the right-rail "ON THIS PAGE" list and `word_count` / `backlink_count` come straight from your index (`tb_pages`, paginated backlinks query in `architecture.md`).

The decorative **waveform** strip is pure CSS; render a fixed bar set (as in the templates) or vary heights server-side — it carries no data.

---

## Accessibility / polish already handled
- `@media (prefers-reduced-motion: reduce)` disables every decorative loop (orbs, equalizer, shimmer, view-enter) — already in `miku.css`.
- Focus styles, hit areas ≥ 40px on controls, and semantic `<a>`/`<button>`/`<form>` elements throughout the templates (real navigation + real POSTs, not div-clicks).
- Color pairs meet contrast in both modes; light-mode accents are slightly deepened (see the `[data-theme='light'][data-accent=…]` overrides).

---

## Files in this handoff
```
design_handoff_miku_redesign/
  README.md                 ← you are here
  preview.html              ← open in a browser: full reading view with real miku.css
  static/
    miku.css               ← the theme. Tokens + every component. Copy into your static/.
    miku.js                ← optional vanilla enhancement (theme/accent persistence, ⌘K/⌘N, slug)
  templates/
    _shell.html             ← topbar + sidebar + <head> (theme restore, fonts, css/js links)
    page_view.html          ← GET /page/:name      (readonly, primary)
    page_edit.html          ← GET /page/:name/edit (textarea form)
    search.html             ← GET /search?q=
    tags.html               ← GET /tags , /tags/:tag
    new_page.html           ← GET /new (+ onboarding empty state)
  Miku Wiki.dc.html         ← original interactive prototype (visual reference only — do not port the runtime)
```

## Naming note
The repo is `miku` but the crate/product is **Miku** (セダム / 麒麟草). The templates say "Miku" in the wordmark. If you want the product itself branded "Miku", change the `.sd-wordmark` text — the theme is identical either way. The *visual* Miku identity (teal→pink, equalizer, holographic) is in the CSS regardless of the product name.
