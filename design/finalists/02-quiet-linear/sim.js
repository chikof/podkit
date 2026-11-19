/* sim.js: simulated product behaviour so the motion can be judged: skeleton -> content, building -> running, live log,
   streamed chat, backup progress, toasts. In podkit all of this arrives as Datastar/SSE patches; none of it is needed for the visuals.
   ?still=1 (or prefers-reduced-motion) skips every timer and renders the final state. */
(() => {
  const d = document, root = d.documentElement, $ = (s, r = d) => r.querySelector(s), $$ = (s, r = d) => [...r.querySelectorAll(s)];
  const q = new URLSearchParams(location.search);
  const still = q.has('still') || root.hasAttribute('data-still');
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches || still;
  const wait = ms => new Promise(r => setTimeout(r, reduce ? 0 : ms));
  const screen = d.body.dataset.screen;

  /* toasts */
  const toast = (msg, err) => { if (still) return; let r = $('.toast-region'); if (!r) { r = d.createElement('div'); r.className = 'toast-region'; r.setAttribute('role', 'status'); d.body.append(r); }
    const t = d.createElement('div'); t.className = 'toast' + (err ? ' toast--err' : ''); t.innerHTML = `<svg class="ic" aria-hidden="true"><use href="#i-${err ? 'circle-alert' : 'circle-check'}"/></svg><span></span>`; t.lastChild.textContent = msg; r.append(t);
    setTimeout(() => { t.dataset.out = ''; setTimeout(() => t.remove(), reduce ? 0 : 260); }, 2800); };
  d.addEventListener('click', e => { const b = e.target.closest('[data-toast]'); if (b) toast(b.dataset.toast); });
  d.addEventListener('submit', e => { if (e.target.dataset.sim === 'invite') { e.preventDefault(); toast('Invite sent'); e.target.reset(); } });

  /* copy + reveal */
  d.addEventListener('click', e => {
    const c = e.target.closest('[data-copy],[data-copy-text]'); if (c) { const v = c.dataset.copyText ?? d.getElementById(c.dataset.copy)?.value; try { navigator.clipboard.writeText(v); } catch (err) {} toast('Copied to clipboard'); }
    const r = e.target.closest('[data-reveal]'); if (r) { const i = d.getElementById(r.dataset.reveal), on = i.type === 'password'; i.type = on ? 'text' : 'password'; r.setAttribute('aria-pressed', on); r.setAttribute('aria-label', (on ? 'Hide ' : 'Show ') + (r.getAttribute('aria-label') || '').replace(/^(Show|Hide) /, '')); r.innerHTML = `<svg class="ic" aria-hidden="true"><use href="#i-${on ? 'eye-off' : 'eye'}"/></svg>`; }
  });

  /* skeleton -> content */
  if (['environment', 'projects'].includes(screen)) {
    const show = () => d.body.removeAttribute('data-loading');
    if (reduce) show(); else { d.body.setAttribute('data-loading', ''); setTimeout(show, 950); }
    $$('[data-reload]').forEach(b => b.addEventListener('click', () => { d.body.setAttribute('data-loading', ''); setTimeout(show, 950); }));
  }

  /* status flip helper */
  const setStatus = (el, st, label) => { el.dataset.status = st; const l = $('.lbl', el); if (l) l.textContent = label; el.setAttribute('data-flip', ''); setTimeout(() => el.removeAttribute('data-flip'), 500); };

  /* environment: checkout-worker finishes building */
  let envRun = 0;
  const envPlay = async () => {
    const card = $('.res[data-id="checkout"]'); if (!card) return; const me = ++envRun, bar = $('.progress>span', card), badge = $('.badge', card), prog = $('.progress', card);
    card.dataset.status = 'building'; setStatus(badge, 'building', 'Building'); prog.hidden = false; bar.style.width = '64%'; await wait(300);
    for (let p = 70; p <= 100; p += 6) { if (me !== envRun) return; bar.style.width = Math.min(p, 100) + '%'; await wait(380); }
    if (me !== envRun) return; await wait(250); card.dataset.status = 'running'; setStatus(badge, 'running', 'Running'); prog.hidden = true; toast('checkout-worker is running');
  };
  if (screen === 'environment') $$('[data-toast="Queued 3 deployments"]').forEach(b => b.addEventListener('click', envPlay));

  /* app: live build log + building -> running */
  const log = $('#build-log'); let run = 0, play = async () => {};
  if (log) {
    const lines = JSON.parse($('#log-script').textContent), pre = 12;
    const mk = l => { const s = d.createElement('span'); s.className = 'log-line'; s.innerHTML = '<span class="ts"></span><span class="lv"></span><span class="tx"></span>'; s.children[0].textContent = l[0]; s.children[1].textContent = l[1]; s.children[1].dataset.lv = l[1]; s.children[2].textContent = l[2]; return s; };
    const badge = $('#deploy-badge'), dep = $('#dep-live');
    const flip = st => { setStatus(badge, st, st === 'running' ? 'Running' : 'Building'); if (dep) { dep.dataset.status = st; $('.tl-state', dep).textContent = st === 'running' ? 'Succeeded in 1m 08s' : 'Building…'; } $('.live-pill')?.toggleAttribute('hidden', st === 'running'); };
    play = async () => { const me = ++run; log.textContent = ''; flip('building'); const n = reduce ? lines.length : pre; for (let i = 0; i < n; i++) log.append(mk(lines[i]));
      for (let i = n; i < lines.length; i++) { await wait(620); if (me !== run) return; log.append(mk(lines[i])); if ($('#follow')?.checked) log.scrollTop = log.scrollHeight; }
      await wait(500); if (me === run) { flip('running'); toast('Deployment #148 succeeded: shop.acme.dev is live'); } };
    play(); $$('[data-redeploy]').forEach(b => b.addEventListener('click', play));
  }

  /* database: back up now */
  $$('[data-backup]').forEach(b => b.addEventListener('click', async () => {
    b.setAttribute('aria-busy', 'true'); await wait(1800); b.removeAttribute('aria-busy');
    const tb = $('.tbl tbody'); if (!tb) return; const tr = tb.firstElementChild.cloneNode(true); tr.children[0].innerHTML = '<b>2026-11-19 13:42</b>'; tr.children[1].textContent = 'manual · chiko'; tr.children[2].textContent = '413 MB'; tb.prepend(tr); tr.animate([{ background: 'var(--accent-soft)' }, { background: 'transparent' }], { duration: reduce ? 1 : 1400 }); toast('Backup completed · 413 MB');
  }));

  /* sign in */
  const si = $('.auth-form');
  if (si) {
    const mail = $('#si-email'), pw = $('#si-pw'), err = $('#si-err'), go = $('#si-go');
    [mail, pw].forEach(i => i.addEventListener('input', () => i.removeAttribute('aria-invalid')));
    if (q.has('error')) { err.hidden = false; mail.value = 'chiko@acme.dev'; pw.setAttribute('aria-invalid', 'true'); }
    si.addEventListener('submit', async e => {
      e.preventDefault(); err.hidden = true; let bad = null;
      [mail, pw].forEach(i => { if (!i.checkValidity()) { i.setAttribute('aria-invalid', 'true'); bad = bad || i; } });
      if (bad) return bad.focus();
      go.setAttribute('aria-busy', 'true'); await wait(900);
      if (pw.value === 'wrong') { go.removeAttribute('aria-busy'); err.hidden = false; pw.setAttribute('aria-invalid', 'true'); pw.focus(); } else location.href = 'projects.html' + location.search;
    });
  }

  /* templates: filter + deploy dialog */
  if (screen === 'templates') {
    const cards = $$('.tpl'), s = $('#tpl-search'); let cat = 'all';
    const apply = () => { const v = (s.value || '').toLowerCase(); let n = 0; cards.forEach(c => { const ok = (cat === 'all' || c.dataset.cat === cat) && c.dataset.name.includes(v); c.hidden = !ok; n += ok; }); $('#tpl-count').textContent = n + (n === 1 ? ' template' : ' templates'); $('.tpl-none').hidden = n > 0; };
    s.addEventListener('input', apply);
    $$('.cats [data-cat]').forEach(b => b.addEventListener('click', () => { cat = b.dataset.cat; $$('.cats [data-cat]').forEach(x => x.setAttribute('aria-pressed', x === b)); apply(); }));
    $$('.tpl-btn').forEach(b => b.addEventListener('click', () => { const c = b.closest('.tpl'), dl = $('#deploy'); $('#deploy-name', dl).textContent = c.dataset.title; $('#deploy-logo', dl).innerHTML = $('.tpl-logo', c).innerHTML; $('#deploy-dom', dl).value = c.dataset.name.replace(/[^a-z0-9]+/g, '-') + '.acme.dev'; }));
    $('#deploy-go')?.addEventListener('click', () => toast('Deploying ' + $('#deploy-name').textContent + ' to storefront / production'));
    if (q.has('open') && !still) setTimeout(() => $('#deploy').showModal(), 350);
  }

  /* playground: streamed answer */
  const ans = $('#answer'); let chat = async () => {};
  if (ans) {
    const think = $('#think'), tstat = $('#think-state'); let n = 0;
    chat = async () => { const me = ++n; ans.innerHTML = ''; ans.dataset.state = 'thinking'; think.open = true; tstat.textContent = 'Thinking…';
      const body = $('.think-body', think), full = body.dataset.full; body.textContent = reduce ? full : '';
      if (!reduce) for (let i = 0; i < full.length; i += 6) { if (me !== n) return; body.textContent = full.slice(0, i + 6); await wait(14); }
      tstat.textContent = 'Thought for 4.1 s'; think.open = false; ans.dataset.state = 'streaming';
      const html = JSON.parse($('#answer-html').textContent), caret = '<span class="caret" aria-hidden="true"></span>';
      if (reduce) ans.innerHTML = html; else { let i = 0, out = '', inTag = false; for (const ch of html) { if (me !== n) return; out += ch; if (ch === '<') inTag = true; if (ch === '>') inTag = false; if (!inTag && ++i % 4 === 0) { ans.innerHTML = out + caret; await wait(11); } } ans.innerHTML = html; }
      ans.dataset.state = 'done'; };
    if (!still) chat(); $('[data-replay]')?.addEventListener('click', chat);
    $('#composer')?.addEventListener('submit', e => { e.preventDefault(); toast('Message queued (prototype)'); });
  }

  /* gallery hook: the parent page can replay the motion in every iframe */
  addEventListener('message', e => { if (e.data && e.data.type === 'play') { envPlay(); play(); chat(); if (screen === 'projects') $('[data-reload]')?.click(); if (screen === 'database') $('[data-backup]')?.click(); } });
  if (q.has('play')) setTimeout(() => { envPlay(); if (screen === 'database') $('[data-backup]')?.click(); }, 600);
})();
