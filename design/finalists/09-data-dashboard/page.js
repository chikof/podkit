/* page.js: interaction layer (about 70 lines). Dialogs, popovers, tabs and tooltips are native HTML (commandfor/command,
   <dialog closedby>, [popover], radio :has() tabs, interestfor); this file only adds what HTML does not do yet:
   theme persistence, Ctrl+K, palette filtering with arrow keys, and fallbacks for browsers that lack invokers,
   focusgroup, interestfor or anchor positioning. Delete it and every screen still renders; menus and dialogs still open. */
(() => {
  const d = document, root = d.documentElement, $ = (s, r = d) => r.querySelector(s), $$ = (s, r = d) => [...r.querySelectorAll(s)];
  const q = new URLSearchParams(location.search);

  /* theme: ?theme= > localStorage > default; carried through local links so the prototype keeps its theme over file:// */
  const carry = () => $$('a[href]').forEach(a => { const h = a.getAttribute('href'); if (!/\.html([?#]|$)/.test(h) || /^(https?:|#)/.test(h)) return; const [p, hash] = h.split('#'); const u = new URL(p, location.href); u.searchParams.set('theme', root.dataset.theme); if (q.has('still')) u.searchParams.set('still', '1'); a.setAttribute('href', p.split('?')[0] + '?' + u.searchParams + (hash ? '#' + hash : '')); });
  const setTheme = t => { root.dataset.theme = t; try { localStorage.setItem('pk-theme', t); } catch (e) {} const u = new URL(location.href); u.searchParams.set('theme', t); history.replaceState(null, '', u); carry(); };
  carry();
  $$('[data-theme-toggle]').forEach(b => b.addEventListener('click', () => setTheme(root.dataset.theme === 'dark' ? 'light' : 'dark')));
  addEventListener('message', e => { if (e.data && e.data.type === 'theme') setTheme(e.data.theme); });

  /* invoker commands fallback (Baseline since Dec 2025) */
  let lastTrigger = null, byKeyboard = false; const popTrig = new WeakMap(), dlgBack = new WeakMap();
  const hasInvokers = 'commandForElement' in HTMLButtonElement.prototype;
  d.addEventListener('click', e => {
    const b = e.target.closest('button[commandfor]'); if (!b) return;
    lastTrigger = b; byKeyboard = e.detail === 0;
    const pop = b.closest('[popover]'); if (pop && b.getAttribute('command') === 'show-modal') dlgBack.set(d.getElementById(b.getAttribute('commandfor')), popTrig.get(pop));
    if (hasInvokers) return;
    const t = d.getElementById(b.getAttribute('commandfor')), c = b.getAttribute('command'); if (!t) return;
    if (c === 'show-modal') t.showModal(); else if (c === 'close') t.close(); else if (c === 'toggle-popover') t.togglePopover();
  });

  /* a dialog opened from a menu item returns focus to the menu's trigger (the item itself is gone by then) */
  d.addEventListener('close', e => { const r = dlgBack.get(e.target); if (r) setTimeout(() => { if (d.activeElement === d.body) r.focus(); }, 0); }, true);

  /* closedby="any" (light dismiss) fallback for Safari */
  if (!('closedBy' in HTMLDialogElement.prototype)) d.addEventListener('click', e => { if (e.target.matches('dialog[closedby="any"]')) e.target.close(); });

  /* popover menus: enter on keyboard open, anchor fallback without CSS anchor positioning */
  const anchored = CSS.supports('position-area', 'block-end');
  d.addEventListener('toggle', e => {
    const m = e.target; if (!m.matches || !m.matches('.menu')) return;
    if (e.newState === 'open') { popTrig.set(m, lastTrigger);
      if (!anchored && lastTrigger) { const r = lastTrigger.getBoundingClientRect(); m.style.cssText += `;position:fixed;inset:auto;margin:0;top:${Math.min(r.bottom + 4, innerHeight - m.offsetHeight - 8)}px;left:${Math.max(8, Math.min(r.left, innerWidth - m.offsetWidth - 8))}px`; }
      if (byKeyboard) $('.menu-item', m)?.focus();
    }
  }, true);
  d.addEventListener('keydown', e => { if (e.key === 'ArrowDown' && e.target.matches?.('[commandfor][aria-haspopup="menu"]')) { const m = d.getElementById(e.target.getAttribute('commandfor')); e.preventDefault(); byKeyboard = true; if (!m.matches(':popover-open')) m.showPopover(); setTimeout(() => $('.menu-item', m)?.focus(), 0); } });
  if (!('focusGroup' in HTMLElement.prototype)) $$('[focusgroup~="menu"]').forEach(g => {
    g.setAttribute('role', 'menu'); $$('.menu-item:not([role])', g).forEach(i => i.setAttribute('role', 'menuitem'));
    g.addEventListener('keydown', e => {
      const items = $$('.menu-item', g), i = items.indexOf(d.activeElement); if (i < 0 || !['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) return;
      e.preventDefault(); items[e.key === 'Home' ? 0 : e.key === 'End' ? items.length - 1 : (i + (e.key === 'ArrowUp' ? -1 : 1) + items.length) % items.length].focus();
    });
  });

  /* tooltips: interestfor fallback */
  if (!('interestForElement' in HTMLButtonElement.prototype)) $$('[interestfor]').forEach(b => {
    const t = d.getElementById(b.getAttribute('interestfor')); if (!t) return;
    const show = () => { t.showPopover(); if (!anchored) { const r = b.getBoundingClientRect(); t.style.cssText += `;position:fixed;inset:auto;margin:0;top:${r.bottom + 6}px;left:${Math.max(8, r.left + r.width / 2 - t.offsetWidth / 2)}px`; } }, hide = () => t.matches(':popover-open') && t.hidePopover();
    b.addEventListener('mouseenter', show); b.addEventListener('focus', show); b.addEventListener('mouseleave', hide); b.addEventListener('blur', hide);
  });

  /* command palette: Ctrl/Cmd+K, filter, arrow keys, Enter */
  const pal = $('#cmdk');
  if (pal) {
    const input = $('#cmd-input'), items = () => $$('#cmd-list li').filter(li => !li.hidden), setSel = i => { const l = items(); $$('#cmd-list a').forEach(a => a.removeAttribute('aria-selected')); const a = l[i] && $('a', l[i]); if (a) { a.setAttribute('aria-selected', 'true'); input.setAttribute('aria-activedescendant', a.id); a.scrollIntoView({ block: 'nearest' }); } else input.removeAttribute('aria-activedescendant'); };
    let sel = 0;
    addEventListener('keydown', e => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); if (pal.open) pal.close(); else pal.showModal(); } });
    pal.addEventListener('toggle', e => { if (e.newState === 'open') { input.value = ''; $$('#cmd-list li').forEach(li => li.hidden = false); $('.cmd-empty', pal).hidden = true; sel = 0; setSel(0); input.focus(); } });
    input.addEventListener('input', () => { const v = input.value.trim().toLowerCase(); $$('#cmd-list li').forEach(li => li.hidden = !li.textContent.toLowerCase().includes(v)); $('.cmd-empty', pal).hidden = items().length > 0; sel = 0; setSel(0); });
    input.addEventListener('keydown', e => { const n = items().length; if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); if (n) { sel = (sel + (e.key === 'ArrowDown' ? 1 : -1) + n) % n; setSel(sel); } } else if (e.key === 'Enter') { const a = items()[sel] && $('a', items()[sel]); if (a) { e.preventDefault(); a.click(); } } });
    pal.addEventListener('close', () => $$('#cmd-list a').forEach(a => a.removeAttribute('aria-selected')));
  }

  /* create picker: preselect from #hash */
  const h = location.hash.slice(1), r = h && $(`input[name="rtype"][value="${CSS.escape(h)}"]`); if (r) r.checked = true;
})();
