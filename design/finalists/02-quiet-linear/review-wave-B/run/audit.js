return (() => {
  const lum = c => { const [r,g,b] = c.map(v => { v/=255; return v<=0.03928? v/12.92 : Math.pow((v+0.055)/1.055,2.4)}); return 0.2126*r+0.7152*g+0.0722*b; };
  const cv=document.createElement('canvas');cv.width=cv.height=1;const cx=cv.getContext('2d',{willReadFrequently:true});const parse = s => { if(!s||s==='transparent') return {c:[0,0,0],a:0}; cx.clearRect(0,0,1,1); cx.fillStyle='#000'; cx.fillStyle=s; cx.fillRect(0,0,1,1); const d=cx.getImageData(0,0,1,1).data; return {c:[d[0],d[1],d[2]], a:d[3]/255}; };
  const bgOf = el => { let stack=[]; for (let e=el; e; e=e.parentElement) { const cs=getComputedStyle(e); if (cs.backgroundImage!=='none') return null; const p=parse(cs.backgroundColor); if (p && p.a>0) { stack.push(p); if (p.a>=1) break; } } let base=[255,255,255]; if(!stack.length||stack[stack.length-1].a<1){ const bp=parse(getComputedStyle(document.documentElement).backgroundColor)||{c:[255,255,255],a:1}; base=bp.c } for (let i=stack.length-1;i>=0;i--){ const s=stack[i]; base = base.map((v,k)=> s.c[k]*s.a+v*(1-s.a)); } return base; };
  const fails = []; let n = 0;
  const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const seen = new Set();
  while (walker.nextNode()) {
    const t = walker.currentNode; if (!t.textContent.trim()) continue;
    const el = t.parentElement; if (!el || seen.has(el)) continue; seen.add(el);
    const cs = getComputedStyle(el); if (cs.visibility==='hidden'||cs.display==='none') continue;
    const r = el.getBoundingClientRect(); if (r.width<1||r.height<1) continue;
    if (el.closest('[hidden],dialog:not([open]),[inert]')) continue;
    const fg = parse(cs.color); const bg = bgOf(el); if(!fg||!bg) continue;
    const a = fg.a; const fgc = fg.c.map((v,k)=> v*a + bg[k]*(1-a));
    const L1 = lum(fgc), L2 = lum(bg); const ratio = (Math.max(L1,L2)+0.05)/(Math.min(L1,L2)+0.05);
    const px = parseFloat(cs.fontSize), bold = parseInt(cs.fontWeight)>=700; const large = px>=24 || (px>=18.66 && bold);
    n++;
    if (ratio < (large?3:4.5)) fails.push({t: t.textContent.trim().slice(0,40), ratio: Math.round(ratio*100)/100, px, el: el.tagName+'.'+String(el.className).slice(0,40)});
  }
  const res = performance.getEntriesByType('resource').map(e=>e.name).filter(u=>!u.startsWith(location.origin));
  const ext = [...document.querySelectorAll('[src],[href]')].map(e=>e.getAttribute('src')||e.getAttribute('href')).filter(u=>u && /^(https?:)?\/\//.test(u) && !u.startsWith(location.origin));
  return {title: document.title, h1: document.querySelectorAll('h1').length, h1text:[...document.querySelectorAll('h1')].map(h=>h.textContent.trim().slice(0,40)),
    overflowX: document.documentElement.scrollWidth > document.documentElement.clientWidth+1, sw: document.documentElement.scrollWidth, cw: document.documentElement.clientWidth,
    contrastChecked: n, contrastFails: fails.slice(0,8), nfails: fails.length, thirdPartyRes: res, externalLinks: ext.slice(0,5),
    errs: window.__qa_errors||[], focusables: document.querySelectorAll('a[href],button,input,select,textarea,[tabindex]').length,
    imgsNoAlt: [...document.querySelectorAll('img:not([alt])')].length, btnNoName: [...document.querySelectorAll('button,a[href]')].filter(b=>!(b.innerText||b.getAttribute('aria-label')||b.title||'').trim()).length };
})();