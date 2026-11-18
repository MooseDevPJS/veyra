// Test helper for a browser started with
// WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222
//   node tools/cdp.mjs <url-substring> "<js expression>"   evaluate JS
//   node tools/cdp.mjs <url-substring> click <x> <y>        real mouse click (user gesture)
//   node tools/cdp.mjs <url-substring> "gesture:<js>"       evaluate as a user action
const [match, ...rest] = process.argv.slice(2);
const targets = await (await fetch('http://127.0.0.1:9222/json')).json();
const t = targets.find((x) => x.type === 'page' && x.url.includes(match));
if (!t) {
    console.log('no page matching', match, '\n' + targets.map((x) => x.url).join('\n'));
    process.exit(1);
}
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r));
let next = 1;
const call = (method, params) => new Promise((resolve) => {
    const id = next++;
    const onMsg = (e) => {
        const m = JSON.parse(e.data);
        if (m.id === id) { ws.removeEventListener('message', onMsg); resolve(m); }
    };
    ws.addEventListener('message', onMsg);
    ws.send(JSON.stringify({ id, method, params }));
});
if (rest[0] === 'click') {
    const [x, y] = rest.slice(1).map(Number);
    for (const type of ['mousePressed', 'mouseReleased']) {
        await call('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
    }
    console.log('clicked', x, y);
} else {
    // "gesture:<js>" evaluates as if the user had just clicked.
    const gesture = rest[0].startsWith('gesture:');
    const expression = gesture ? rest[0].slice(8) : rest[0];
    const m = await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true, userGesture: gesture });
    console.log(JSON.stringify(m.result?.result?.value ?? m.result ?? m.error, null, 1));
}
ws.close();
