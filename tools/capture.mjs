// Screenshot of a page's content straight from the engine (other windows
// on top do not matter). Needs BROWSER_DEVTOOLS_PORT=9222.
//   node tools/capture.mjs <url-substring> <out.png> [width height scale transparent]
import { writeFileSync } from 'node:fs';
const [match, out, w, h, scale = '1', transparent = ''] = process.argv.slice(2);
const targets = await (await fetch('http://127.0.0.1:9222/json')).json();
const t = targets.find((x) => x.type === 'page' && x.url.includes(match));
if (!t) { console.log('no page matching', match); process.exit(1); }
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r));
let next = 1;
const call = (method, params = {}) => new Promise((resolve) => {
  const id = next++;
  const on = (e) => { const m = JSON.parse(e.data); if (m.id === id) { ws.removeEventListener('message', on); resolve(m.result); } };
  ws.addEventListener('message', on);
  ws.send(JSON.stringify({ id, method, params }));
});
if (transparent) await call('Emulation.setDefaultBackgroundColorOverride', { color: { r: 0, g: 0, b: 0, a: 0 } });
const params = { format: 'png', captureBeyondViewport: true };
if (w && h) params.clip = { x: 0, y: 0, width: +w, height: +h, scale: +scale };
const r = await call('Page.captureScreenshot', params);
writeFileSync(out, Buffer.from(r.data, 'base64'));
console.log('saved', out);
ws.close();
