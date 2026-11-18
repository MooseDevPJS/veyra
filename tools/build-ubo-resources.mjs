// Converts uBlock Origin's scriptlets and redirect resources into the JSON
// format adblock-rust's `Engine::use_resources` expects.
//
// Usage: node tools/build-ubo-resources.mjs <uBlock-src-dir> <out.json>
//   <uBlock-src-dir> is the `src` folder of github.com/gorhill/uBlock.
//
// The uBO sources are only READ AS TEXT and parsed (never imported or run).
// The output contains uBO code (GPLv3); it ships as a separate data file.

import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';

const [srcDir, outFile] = process.argv.slice(2);
if (!srcDir || !outFile) {
    console.error('usage: build-ubo-resources.mjs <uBlock/src> <out.json>');
    process.exit(1);
}
const b64 = (s) => Buffer.from(s).toString('base64');
const TRUSTED = 1; // PermissionMask bit granted to uBO's own lists

// Index just past the bracket matching the one at `open`, skipping strings,
// template literals, comments and regex literals.
function matchBracket(src, open) {
    const pairs = { '(': ')', '{': '}', '[': ']' };
    const stack = [pairs[src[open]]];
    let i = open + 1;
    let prev = src[open]; // last significant char, to tell regex from division
    while (i < src.length && stack.length) {
        const c = src[i];
        if (c === '/' && src[i + 1] === '/') { i = src.indexOf('\n', i); continue; }
        if (c === '/' && src[i + 1] === '*') { i = src.indexOf('*/', i) + 2; continue; }
        if (c === '"' || c === "'") {
            i++;
            while (src[i] !== c) i += src[i] === '\\' ? 2 : 1;
            i++; prev = c; continue;
        }
        if (c === '`') {
            i++;
            while (src[i] !== '`') {
                if (src[i] === '\\') { i += 2; continue; }
                if (src[i] === '$' && src[i + 1] === '{') { i = matchBracket(src, i + 1); continue; }
                i++;
            }
            i++; prev = '`'; continue;
        }
        if (c === '/' && '(,=:[!&|?{};+-*%<>~^'.includes(prev) || c === '/' && /\breturn\s*$/.test(src.slice(Math.max(0, i - 8), i))) {
            i++;
            let inClass = false;
            while (src[i] !== '/' || inClass) {
                if (src[i] === '\\') { i += 2; continue; }
                if (src[i] === '[') inClass = true;
                else if (src[i] === ']') inClass = false;
                i++;
            }
            i++;
            while (/[a-z]/i.test(src[i])) i++;
            prev = '/'; continue;
        }
        if (pairs[c]) stack.push(pairs[c]);
        else if (c === stack[stack.length - 1]) stack.pop();
        if (!/\s/.test(c)) prev = c;
        i++;
    }
    return i;
}

const functions = new Map(); // js identifier -> source text
const registrations = [];    // { ident, details }
// Scriptlets live in js/resources; a few helpers they import sit in js/.
const files = ['js/resources', 'js'].flatMap((d) =>
    fs.readdirSync(path.join(srcDir, d)).filter((f) => f.endsWith('.js')).map((f) => path.join(srcDir, d, f)));
for (const file of files) {
    const src = fs.readFileSync(file, 'utf8');
    for (const m of src.matchAll(/^(?:export\s+)?((?:async\s+)?function\s+([\w$]+)\s*\(|class\s+([\w$]+)\s*\{)/gm)) {
        const ident = m[2] ?? m[3];
        if (functions.has(ident)) continue;
        const start = m.index + m[0].indexOf(m[1]);
        const bodyStart = m[2]
            ? src.indexOf('{', matchBracket(src, src.indexOf('(', start)))
            : src.indexOf('{', start);
        functions.set(ident, src.slice(start, matchBracket(src, bodyStart)));
    }
    for (const m of src.matchAll(/registerScriptlet\(\s*([\w$]+)\s*,\s*\{/g)) {
        const open = m.index + m[0].length - 1;
        registrations.push({ ident: m[1], details: src.slice(open, matchBracket(src, open)) });
    }
}

const strings = (text) => [...(text ?? '').matchAll(/'([^']*)'/g)].map((x) => x[1]);
const field = (details, key) => details.match(new RegExp(`\\b${key}\\s*:\\s*(\\[[^\\]]*\\]|[^,\\n]+)`))?.[1];

const nameOf = new Map(); // js identifier -> registered resource name
for (const r of registrations) nameOf.set(r.ident, strings(field(r.details, 'name'))[0]);

const resources = [];
let failed = 0;
for (const { ident, details } of registrations) {
    const name = nameOf.get(ident);
    const code = functions.get(ident);
    try {
        if (!name || !code) throw new Error('missing name or function');
        new vm.Script(`(${code})`); // syntax check only, nothing runs
    } catch (e) {
        console.error(`skip ${ident}: ${e.message}`);
        failed++;
        continue;
    }
    const deps = (details.match(/dependencies\s*:\s*\[([^\]]*)\]/)?.[1] ?? '')
        .split(',').map((s) => s.trim()).filter(Boolean)
        .map((d) => (d.startsWith("'") ? d.slice(1, -1) : nameOf.get(d)))
        .filter(Boolean);
    const r = {
        name,
        aliases: strings(field(details, 'aliases')),
        kind: { mime: name.endsWith('.fn') ? 'fn/javascript' : 'application/javascript' },
        content: b64(code),
    };
    if (deps.length) r.dependencies = deps;
    if (/requiresTrust\s*:\s*true/.test(details)) r.permission = TRUSTED;
    resources.push(r);
}

// Redirect resources (noop.js, 1x1.gif, google-ima.js, ...) listed in the
// Map literal of redirect-resources.js.
const mime = {
    '.css': 'text/css', '.gif': 'image/gif', '.html': 'text/html',
    '.js': 'application/javascript', '.json': 'application/json',
    '.mp3': 'audio/mp3', '.mp4': 'video/mp4', '.png': 'image/png',
    '.txt': 'text/plain', '.xml': 'text/xml',
};
const redirectSrc = fs.readFileSync(path.join(srcDir, 'js/redirect-resources.js'), 'utf8');
const war = path.join(srcDir, 'web_accessible_resources');
let redirects = 0;
for (const m of redirectSrc.matchAll(/\[\s*'([^']+)'\s*,\s*\{([^}]*)\}\s*\]/g)) {
    const file = path.join(war, m[1]);
    if (!fs.existsSync(file)) continue;
    const alias = m[2].match(/alias\s*:\s*(\[[^\]]*\]|'[^']*')/)?.[1];
    resources.push({
        name: m[1],
        aliases: strings(alias),
        kind: { mime: mime[path.extname(m[1])] ?? 'application/octet-stream' },
        content: fs.readFileSync(file).toString('base64'),
    });
    redirects++;
}

fs.writeFileSync(outFile, JSON.stringify(resources));
console.log(`${resources.length - redirects} scriptlets + ${redirects} redirects -> ${outFile} (${failed} skipped)`);
