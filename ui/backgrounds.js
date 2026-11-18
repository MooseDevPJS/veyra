// New tab page backgrounds, shared by newtab.html and settings.html.
// A choice is "image" (the user's own file, image or video, served at
// background/<file>), "color:N" or "gradient:N".
const VEYRA_BG = {
  colors: ['#1C1C1F', '#2E3440', '#1E2A44', '#1F3A33', '#3A2430', '#4A3F35', '#C9CED6', '#E4DED3'],
  gradients: [
    'linear-gradient(135deg, #232526 0%, #414345 100%)',
    'linear-gradient(135deg, #141E30 0%, #243B55 100%)',
    'linear-gradient(135deg, #0F2027 0%, #203A43 50%, #2C5364 100%)',
    'linear-gradient(135deg, #3A1C71 0%, #7B4397 55%, #D76D77 100%)',
    'linear-gradient(135deg, #42275A 0%, #734B6D 100%)',
    'linear-gradient(135deg, #D7D2CC 0%, #304352 100%)',
  ],
};

// CSS `background` for a choice (`url`: the user's image).
function bgCss(choice, url) {
  const [kind, n] = String(choice || '').split(':');
  if (kind === 'image') return url ? `center / cover no-repeat url("${url}")` : '';
  if (kind === 'color') return VEYRA_BG.colors[+n] || VEYRA_BG.colors[0];
  if (kind === 'gradient') return VEYRA_BG.gradients[+n] || VEYRA_BG.gradients[0];
  return '';
}

const bgIsVideo = (u) => /\.(mp4|m4v|webm|mov)$/i.test(u || '');

// English for the picker's texts (keyed by the Italian).
const BG_EN = {
  'Sfondo della pagina': 'Page background', 'Immagine o video': 'Image or video', 'Tinta unita': 'Solid color',
  'Gradiente': 'Gradient', 'Cambia': 'Change',
};

// The picker (three cards, swatches, "Change"), built into `box`:
//   t(s)      translates a text
//   choice()  the current choice; pick(v) chooses one
//   file()    the user's image or video (URL) or null
//   choose()  asks the host for a new file (Windows "Open" dialog)
function createBgPicker(box, { t, choice, pick, file, choose }) {
  box.innerHTML = `
    <div class="bg-cards" role="group" aria-label="${t('Sfondo della pagina')}">
      <button type="button" class="bg-card" data-kind="image"><span class="bg-thumb"><svg class="bg-add" aria-hidden="true" viewBox="0 0 16 16" fill="none" stroke-width="1.4" stroke-linecap="round"><path d="M8 3.5v9M3.5 8h9"/></svg></span><span class="bg-name">${t('Immagine o video')}</span></button>
      <button type="button" class="bg-card" data-kind="color"><span class="bg-thumb"></span><span class="bg-name">${t('Tinta unita')}</span></button>
      <button type="button" class="bg-card" data-kind="gradient"><span class="bg-thumb"></span><span class="bg-name">${t('Gradiente')}</span></button>
    </div>
    <div class="bg-swatches" role="group"></div>
    <div class="bg-image-actions" hidden><button type="button" class="quiet bg-change">${t('Cambia')}</button></div>`;
  const $ = (s) => box.querySelector(s);
  const render = () => {
    const [kind] = String(choice() || '').split(':');
    const url = file();
    for (const c of box.querySelectorAll('.bg-card')) c.setAttribute('aria-pressed', String(c.dataset.kind === kind));
    const thumbs = box.querySelectorAll('.bg-thumb');
    // The user's file in its card: a still image, or the video playing muted.
    const own = thumbs[0];
    const vid = own.querySelector('video');
    if (bgIsVideo(url)) {
      own.style.background = '';
      if (vid?.getAttribute('src') !== url) {
        vid?.remove();
        own.insertAdjacentHTML('afterbegin', `<video src="${url}" muted loop playsinline preload="auto"></video>`);
      }
    } else {
      vid?.remove();
      own.style.background = url ? bgCss('image', url) : '';
    }
    $('.bg-add').style.display = url ? 'none' : '';
    thumbs[1].style.background = bgCss(kind === 'color' ? choice() : 'color:1');
    thumbs[2].style.background = bgCss(kind === 'gradient' ? choice() : 'gradient:1');
    const sw = $('.bg-swatches');
    sw.textContent = '';
    if (kind === 'color' || kind === 'gradient') {
      const list = kind === 'color' ? VEYRA_BG.colors : VEYRA_BG.gradients;
      list.forEach((_, i) => {
        const v = `${kind}:${i}`;
        const b = document.createElement('button');
        b.type = 'button'; b.className = 'bg-swatch'; b.style.background = bgCss(v);
        b.setAttribute('aria-pressed', String(choice() === v));
        b.setAttribute('aria-label', `${t(kind === 'color' ? 'Tinta unita' : 'Gradiente')} ${i + 1}`);
        b.addEventListener('click', () => { pick(v); render(); });
        sw.append(b);
      });
    }
    $('.bg-image-actions').hidden = kind !== 'image' || !url;
  };
  for (const c of box.querySelectorAll('.bg-card')) {
    c.addEventListener('click', () => {
      const kind = c.dataset.kind;
      if (kind === 'image') {
        if (!file()) return choose();
        pick('image');
      } else if (!String(choice()).startsWith(kind + ':')) {
        pick(`${kind}:1`);
      }
      render();
    });
  }
  $('.bg-change').addEventListener('click', () => choose());
  return { render };
}
