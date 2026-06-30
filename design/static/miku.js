/* ============================================================================
   miku.js — optional progressive enhancement. No bundler, no framework.
   Add with a single  <script defer src="/static/miku.js"></script>  in the shell.
   Everything here is OPTIONAL: the server-rendered UI works without it. This
   only adds theme/accent persistence and a couple of keyboard shortcuts.

   NOTE: live side-by-side Markdown preview is intentionally NOT here — per
   docs/architecture.md it is roadmap and pairs with CodeMirror 6. The edit
   view ships as the classic textarea until then.
   ============================================================================ */
(function () {
  'use strict';
  var root = document.documentElement;
  var LS_THEME = 'miku:theme';
  var LS_ACCENT = 'miku:accent';

  /* ---- Restore persisted theme + accent (FOUC-safe if you also inline the
     two getItem lines in <head>; see README) ---------------------------- */
  function apply(attr, key, fallback) {
    var v = null;
    try { v = localStorage.getItem(key); } catch (e) {}
    root.setAttribute(attr, v || fallback);
  }
  apply('data-theme', LS_THEME, 'dark');
  apply('data-accent', LS_ACCENT, 'miku');

  function setTheme(mode) {
    root.setAttribute('data-theme', mode);
    try { localStorage.setItem(LS_THEME, mode); } catch (e) {}
    syncActive();
  }
  function setAccent(name) {
    root.setAttribute('data-accent', name);
    try { localStorage.setItem(LS_ACCENT, name); } catch (e) {}
    syncActive();
  }

  /* ---- Reflect current state onto controls (data-set-theme / data-set-accent) */
  function syncActive() {
    var theme = root.getAttribute('data-theme');
    var accent = root.getAttribute('data-accent');
    document.querySelectorAll('[data-set-theme]').forEach(function (el) {
      el.classList.toggle('is-active', el.getAttribute('data-set-theme') === theme);
    });
    document.querySelectorAll('[data-set-accent]').forEach(function (el) {
      el.classList.toggle('is-active', el.getAttribute('data-set-accent') === accent);
    });
  }

  document.addEventListener('click', function (e) {
    var t = e.target.closest('[data-set-theme]');
    if (t) { setTheme(t.getAttribute('data-set-theme')); return; }
    var a = e.target.closest('[data-set-accent]');
    if (a) { setAccent(a.getAttribute('data-set-accent')); return; }
  });

  /* ---- Keyboard: Cmd/Ctrl-K → search, Cmd/Ctrl-N → new page ----------- */
  document.addEventListener('keydown', function (e) {
    if (!(e.metaKey || e.ctrlKey)) return;
    var k = e.key.toLowerCase();
    if (k === 'k') {
      var s = document.querySelector('[data-go-search]');
      if (s) { e.preventDefault(); (s.href ? (location.href = s.href) : s.click()); }
    } else if (k === 'n') {
      var n = document.querySelector('[data-go-new]');
      if (n) { e.preventDefault(); (n.href ? (location.href = n.href) : n.click()); }
    }
  });

  /* ---- Create-page dialog: live-slug the path as the title is typed ----
     Markup contract: an input[data-create-title], a folder selector group of
     [data-folder] elements (data-folder = "" | "guides/" ...), and a
     [data-create-path] element to render "<folder><slug>.md" into.          */
  function slug(s) {
    return (s || '').trim().toLowerCase()
      .replace(/[^a-z0-9\u00C0-\uFFFF]+/g, '-')
      .replace(/^-+|-+$/g, '') || 'untitled-page';
  }
  var titleEl = document.querySelector('[data-create-title]');
  if (titleEl) {
    var pathEl = document.querySelector('[data-create-path]');
    var folder = '';
    function repaint() { if (pathEl) pathEl.textContent = folder + slug(titleEl.value) + '.md'; }
    titleEl.addEventListener('input', repaint);
    document.querySelectorAll('[data-folder]').forEach(function (el) {
      el.addEventListener('click', function () {
        folder = el.getAttribute('data-folder') || '';
        document.querySelectorAll('[data-folder]').forEach(function (x) { x.classList.toggle('is-active', x === el); });
        repaint();
      });
    });
    repaint();
  }

  syncActive();
})();
