/* page.js: tiny vanilla enhancement layer. Everything is a progressive enhancement of server-rendered HTML;
   in podkit this logic maps to Datastar attributes (data-on:click, data-show, SSE patches). */
(() => {
  const d = document, root = d.documentElement;
  const q = new URLSearchParams(location.search);
  const still = q.has('still');
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches || still;
  const $ = (s, r = d) => r.querySelector(s), $$ = (s, r = d) => [...r.querySelectorAll(s)];
  const wait = ms => new Promise(r => setTimeout(r, reduce ? 0 : ms));

  /* theme: ?theme= > localStorage > concept default; carried across pages via ?theme= on local links */
  const carry = () => { const t = root.dataset.theme; $$('a[href$=".html"],a[href*=".html?"],a[href*=".html#"]').forEach(a => { const h = a.getAttribute('href'); if (/^(https?:|#)/.test(h)) return; const [p, hash] = h.split('#'); const u = new URL(p, location.href); u.searchParams.set('theme', t); if (still) u.searchParams.set('still', '1'); a.href = u.pathname.split('/').pop() + '?' + u.searchParams + (hash ? '#' + hash : ''); }); };
  carry();
  $$('[data-theme-toggle]').forEach(b => b.addEventListener('click', () => {
    const t = root.dataset.theme === 'dark' ? 'light' : 'dark';
    root.dataset.theme = t; try { localStorage.setItem('pk-theme', t); } catch (e) {}
    const u = new URL(location.href); u.searchParams.set('theme', t); history.replaceState(null, '', u); carry();
  }));

  /* top-layer helpers (exit animation works everywhere via data-closing) */
  const closeTop = async el => {
    if (!el.open) return;
    el.setAttribute('data-closing', '');
    const an = el.getAnimations({ subtree: true });
    await Promise.race([Promise.allSettled(an.map(a => a.finished)), wait(400)]);
    await new Promise(r => setTimeout(r, reduce ? 0 : parseFloat(getComputedStyle(el).transitionDuration) * 1000 || 0));
    el.removeAttribute('data-closing'); el.close();
  };
  $$('dialog').forEach(dl => {
    dl.addEventListener('cancel', e => { e.preventDefault(); closeTop(dl); });
    dl.addEventListener('click', e => { if (e.target === dl) closeTop(dl); });
    $$('[data-close]', dl).forEach(b => b.addEventListener('click', e => { e.preventDefault(); closeTop(dl); }));
  });
  $$('[data-open]').forEach(b => b.addEventListener('click', () => { const dl = d.getElementById(b.dataset.open); if (dl && !dl.open) { dl.showModal(); const f = $('input,select,button.btn--primary', dl); f && f.focus(); } }));
  addEventListener('keydown', e => { if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') { e.preventDefault(); const dl = $('#cmdk'); dl && (dl.open ? closeTop(dl) : (dl.showModal(), $('input', dl).focus())); } });

  /* command palette filter */
  const cmdIn = $('#cmdk input');
  if (cmdIn) cmdIn.addEventListener('input', () => { const v = cmdIn.value.toLowerCase(); $$('#cmdk li').forEach(li => li.hidden = !li.textContent.toLowerCase().includes(v)); });

  /* toast */
  const toast = msg => { if (still) return; let r = $('.toast-region'); if (!r) { r = d.createElement('div'); r.className = 'toast-region'; r.setAttribute('role', 'status'); d.body.append(r); } const t = d.createElement('div'); t.className = 'toast'; t.innerHTML = '<svg class="ic" aria-hidden="true"><use href="#i-circle-check"/></svg><span></span>'; t.lastChild.textContent = msg; r.append(t); setTimeout(() => { t.dataset.out = ''; setTimeout(() => t.remove(), reduce ? 0 : 260); }, 2600); };
  $$('[data-toast]').forEach(b => b.addEventListener('click', () => toast(b.dataset.toast)));

  /* environment: skeleton → content */
  if (d.body.dataset.screen === 'environment' || d.body.dataset.screen === 'projects') {
    const show = () => d.body.removeAttribute('data-loading');
    if (reduce) show(); else { d.body.setAttribute('data-loading', ''); setTimeout(show, 950); }
    $$('[data-reload]').forEach(b => b.addEventListener('click', () => { d.body.setAttribute('data-loading', ''); setTimeout(show, 950); }));
  }

  /* template gallery: filter + deploy dialog */
  if (d.body.dataset.screen === 'templates') {
    const cards = $$('.tpl'), s = $('#tpl-search'); let cat = 'all';
    const apply = () => { const v = (s.value || '').toLowerCase(); let n = 0; cards.forEach(c => { const ok = (cat === 'all' || c.dataset.cat === cat) && c.dataset.name.includes(v); c.hidden = !ok; n += ok; }); $('#tpl-count').textContent = n + (n === 1 ? ' template' : ' templates'); };
    s.addEventListener('input', apply);
    $$('[data-cat]', $('.cats')).forEach(b => b.addEventListener('click', () => { cat = b.dataset.cat; $$('[data-cat]', $('.cats')).forEach(x => x.setAttribute('aria-pressed', x === b)); apply(); }));
    $$('.tpl [data-open]').forEach(b => b.addEventListener('click', () => { const c = b.closest('.tpl'); const dl = $('#deploy'); $('#deploy-name', dl).textContent = c.dataset.title; $('#deploy-logo', dl).innerHTML = $('.tpl-logo', c).innerHTML; $('#deploy-dom', dl).value = c.dataset.name.replace(/[^a-z0-9]+/g, '-') + '.acme.dev'; }));
    const dl = $('#deploy'); if (dl && !q.has('closed')) setTimeout(() => dl.showModal(), reduce ? 0 : 350);
    $('#deploy-go')?.addEventListener('click', () => toast('Deploying ' + $('#deploy-name').textContent + ' to storefront / production'));
  }

  /* app: live build log + building → running */
  const log = $('#build-log');
  if (log) {
    const lines = JSON.parse($('#log-script').textContent); const pre = 12;
    const mk = (l, i) => { const s = d.createElement('span'); s.className = 'log-line'; s.innerHTML = '<span class="ts"></span><span class="lv"></span><span class="tx"></span>'; s.children[0].textContent = l[0]; s.children[1].textContent = l[1]; s.children[1].dataset.lv = l[1]; s.children[2].textContent = l[2]; return s; };
    const badge = $('#deploy-badge'), dep = $('#dep-live');
    const setStatus = st => { badge.dataset.status = st; $('.lbl', badge).textContent = st === 'running' ? 'Running' : 'Building'; badge.setAttribute('data-flip', ''); setTimeout(() => badge.removeAttribute('data-flip'), 500); if (dep) { dep.dataset.status = st; $('.tl-state', dep).textContent = st === 'running' ? 'Succeeded in 1m 08s' : 'Building…'; } $('.live-pill')?.toggleAttribute('hidden', st === 'running'); };
    let run = 0;
    const play = async () => {
      const me = ++run; log.textContent = ''; setStatus('building');
      const n = reduce ? lines.length : pre;
      for (let i = 0; i < n; i++) log.append(mk(lines[i], i));
      for (let i = n; i < lines.length; i++) { await wait(620); if (me !== run) return; log.append(mk(lines[i], i)); if ($('#follow')?.checked) log.scrollTop = log.scrollHeight; }
      await wait(500); if (me === run) { setStatus('running'); toast('Deployment #148 succeeded: shop.acme.dev is live'); }
    };
    if (reduce) { play(); } else play();
    $$('[data-redeploy]').forEach(b => b.addEventListener('click', play));
  }

  /* playground: streamed answer */
  const ans = $('#answer');
  if (ans) {
    const src = ans.dataset.full; const think = $('#think'); const tstat = $('#think-state'); const tps = $('#live-tps');
    const run = async () => {
      ans.innerHTML = ''; ans.dataset.state = 'thinking'; think.open = true; if (tstat) tstat.textContent = 'Thinking…';
      const body = $('.think-body', think); const full = body.dataset.full; body.textContent = reduce ? full : '';
      if (!reduce) for (let i = 0; i < full.length; i += 6) { body.textContent = full.slice(0, i + 6); await wait(14); }
      if (tstat) tstat.textContent = 'Thought for 4.1 s'; think.open = false; ans.dataset.state = 'streaming';
      const html = JSON.parse($('#answer-html').textContent); const caret = '<span class="caret" aria-hidden="true"></span>';
      if (reduce) ans.innerHTML = html; else { let i = 0; const tmp = d.createElement('div'); tmp.innerHTML = html; const text = tmp.textContent.length; /* reveal by html prefix, skipping tags */ let out = ''; let inTag = false; for (const ch of html) { out += ch; if (ch === '<') inTag = true; if (ch === '>') inTag = false; if (!inTag && ++i % 4 === 0) { ans.innerHTML = out + caret; await wait(11); } } ans.innerHTML = html; }
      ans.dataset.state = 'done'; tps && (tps.textContent = '175');
    };
    run(); $('[data-replay]')?.addEventListener('click', run);
    think.addEventListener('toggle', () => { const sm = $('summary .ic', think); });
    $('#composer')?.addEventListener('submit', e => { e.preventDefault(); toast('Message queued (mockup)'); });
  }

  /* concept card: live token readout */
  const cs = getComputedStyle(root);
  $$('.sw-hex').forEach(el => { const p = d.createElement('i'); p.style.color = `var(${el.dataset.var})`; d.body.append(p); const c = getComputedStyle(p).color; p.remove(); const m = c.match(/[\d.]+/g); el.textContent = c.startsWith('rgb') && m ? '#' + m.slice(0, 3).map(n => (+n | 0).toString(16).padStart(2, '0')).join('') : c; });
  const mt = $('#motion-tokens');
  if (mt) mt.innerHTML = ['--dur-1', '--dur-2', '--dur-3', '--dur-vt', '--ease', '--ease-out', '--ease-spring', '--stagger'].map(k => `<div><dt>${k}</dt><dd>${cs.getPropertyValue(k).trim().replace(/\s+/g, ' ')}</dd></div>`).join('');
})();
