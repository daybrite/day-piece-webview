// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// Run with: node --test tests/browser.mjs
// Exercise the shipped arm without requiring a Wasm build or a browser installation.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { runInNewContext } from 'node:vm';

const source = readFileSync(new URL('../src/browser.rs', import.meta.url), 'utf8');
const arm = source.match(/js!\(r#"([\s\S]*?)"#\);/)[1];

function setup() {
    const events = [];
    const frame = new EventTarget();
    let release;
    const attach = runInNewContext(`${arm}\nattach_browser`, {
        URL,
        crypto: {getRandomValues: a=>a.fill(123)},
        Uint32Array,
        navigator: {},
        document: { baseURI: 'https://example.test/app/' },
        dayHost: { dom: {
            element: () => frame,
            emit: (...args) => events.push(args),
            onRelease: (_id, dispose) => { release = dispose; },
        } },
    });
    attach(7, 'assets/data/site/');
    function load(path = 'index.html') {
        const doc = new EventTarget();
        doc.baseURI = `https://example.test/app/assets/data/site/${path}`;
        frame.contentDocument = doc;
        frame.dispatchEvent(new Event('load'));
        return doc;
    }
    return { frame, events, load, release: () => release() };
}

function click(doc, href) {
    const event = new Event('click', { cancelable: true });
    Object.defineProperty(event, 'target', { value: {
        closest: () => ({ getAttribute: () => href }),
    } });
    doc.dispatchEvent(event);
    return event.defaultPrevented;
}

test('bundled links navigate; outside links become app events', () => {
    const s = setup();
    const doc = s.load();
    for (const href of ['next.html', '#anchor', '?query=1', 'about:blank']) {
        assert.equal(click(doc, href), false, href);
    }
    for (const href of ['webviewdemo://greeting', 'https://elsewhere.test/', '../site-other/index.html']) {
        assert.equal(click(doc, href), true, href);
        assert.deepEqual(s.events.at(-1), [7, -1, new URL(href, doc.baseURI).href]);
    }
});

test('navigation replaces listeners and release removes document and frame hooks', () => {
    const s = setup();
    const old = s.load();
    const current = s.load('next.html');
    assert.equal(click(old, 'webviewdemo://stale'), false);
    assert.equal(click(current, 'webviewdemo://current'), true);
    assert.equal(s.events.length, 1);
    s.release();
    assert.equal(click(current, 'webviewdemo://released'), false);
    assert.equal(click(s.load(), 'webviewdemo://after-release'), false);
    assert.equal(s.events.length, 1);
});

test('inaccessible frame documents do not break the host', () => {
    const s = setup();
    const old = s.load();
    Object.defineProperty(s.frame, 'contentDocument', {
        get() { throw new Error('Cross-origin document'); },
    });
    s.frame.dispatchEvent(new Event('load'));
    assert.equal(click(old, 'webviewdemo://stale'), false);
    s.release();
});

test('same-origin evaluation delivers the wrapped result to the matching request', () => {
    const events = [];
    const frame = { contentDocument: {}, contentWindow: { eval: script => {
        assert.equal(script, 'wrapped script'); return '1\u001f42';
    } } };
    const evaluate = runInNewContext(`${arm}\neval_browser`, {
        crypto: {getRandomValues: a=>a.fill(123)}, Uint32Array, navigator: {},
        dayHost: { dom: { element: () => frame, emit: (...args) => events.push(args) } },
    });
    evaluate(7, 123, 'wrapped script');
    assert.deepEqual(events, [[7, 123, '1\u001f42']]);
});

test('cross-origin or destroyed frames answer with an error instead of stranding evaluation', () => {
    const events = [];
    const evaluate = runInNewContext(`${arm}\neval_browser`, {
        crypto: {getRandomValues: a=>a.fill(123)}, Uint32Array, navigator: {},
        dayHost: { dom: { element: () => ({ contentDocument: null }), emit: (...args) => events.push(args) } },
    });
    evaluate(8, 124, 'anything');
    assert.equal(events[0][0], 8);
    assert.equal(events[0][1], 124);
    assert.match(events[0][2], /^0\u001fError\u001f.*cross-origin/);
});

// Load reporting is independent of the optional bundled-site click policy. Each real load
// reports twice: the URL (num 0) and the page state (num -5: loading/back/forward flags,
// progress, URL, and the title where the frame is same-origin).
test('load reports skip startup blank, repeat for reload, support cross-origin, and dispose', () => {
    const frame = new EventTarget(), events = [];
    let dispose;
    const observe = runInNewContext(`${arm}\nobserve_browser_load`, {
        URL, Uint32Array, crypto: {getRandomValues: a=>a.fill(123)}, navigator: {},
        dayHost: {dom: {
            element: () => frame,
            emit: (...args) => events.push(args),
            onRelease: (_id, fn) => { dispose = fn; },
        }},
    });
    const state = (url, title) => [7, -5, `000\u001f1\u001f${url}\u001f${title}`];
    observe(7);
    frame.src = 'https://example.test/reader.html';
    frame.contentWindow = {location: {href: 'about:blank'}};
    frame.dispatchEvent(new Event('load'));
    assert.deepEqual(events, [], 'the resource worker\'s startup blank is not a load');

    frame.contentWindow.location.href = frame.src;
    frame.contentDocument = {title: 'Reader'};
    frame.dispatchEvent(new Event('load'));
    assert.deepEqual(events, [[7, 0, frame.src], state(frame.src, 'Reader')]);

    // A reload reports again, both events, with the title the reloaded page has now.
    frame.contentDocument = {title: 'Reader (2)'};
    frame.dispatchEvent(new Event('load'));
    assert.deepEqual(events.slice(2), [[7, 0, frame.src], state(frame.src, 'Reader (2)')]);

    // Cross-origin: the URL comes from `src` and the title is unknowable, so it is empty.
    frame.src = 'https://publication.test/article';
    Object.defineProperty(frame.contentWindow, 'location', {get() { throw new Error('cross-origin'); }});
    Object.defineProperty(frame, 'contentDocument', {get() { throw new Error('cross-origin'); }});
    frame.dispatchEvent(new Event('load'));
    assert.deepEqual(events.slice(4), [[7, 0, frame.src], state(frame.src, '')]);

    dispose();
    frame.dispatchEvent(new Event('load'));
    assert.equal(events.length, 6, 'nothing reports after disposal');
});

test('zoom follows same-origin loads, and cross-origin documents stay untouched', () => {
    const frame = new EventTarget();
    let dispose;
    const zoom = runInNewContext(`${arm}\nzoom_browser`, {
        URL, Uint32Array, crypto: {getRandomValues: a=>a.fill(123)}, navigator: {},
        dayHost: {dom: { element: () => frame, onRelease: (_id, fn) => { dispose = fn; } }},
    });
    const page = () => ({documentElement: {style: {zoom: ''}}});
    frame.contentDocument = page();
    zoom(7, 1.5);
    assert.equal(frame.contentDocument.documentElement.style.zoom, '1.5');
    // The next document is zoomed when it loads; one listener however often the factor changes.
    zoom(7, 2);
    frame.contentDocument = page();
    frame.dispatchEvent(new Event('load'));
    assert.equal(frame.contentDocument.documentElement.style.zoom, '2');
    zoom(7, 1);
    assert.equal(frame.contentDocument.documentElement.style.zoom, '');
    Object.defineProperty(frame, 'contentDocument', {get() { throw new Error('cross-origin'); }});
    assert.doesNotThrow(() => zoom(7, 3));
    assert.doesNotThrow(() => frame.dispatchEvent(new Event('load')));
    dispose();
});

test('page scripts off sandboxes the frame without allow-scripts, and on lifts the sandbox', () => {
    const attributes = new Map();
    const frame = {
        setAttribute: (name, value) => attributes.set(name, value),
        removeAttribute: name => attributes.delete(name),
    };
    const scripts = runInNewContext(`${arm}\nscripts_browser`, {
        URL, Uint32Array, crypto: {getRandomValues: a=>a.fill(123)}, navigator: {},
        dayHost: {dom: { element: () => frame }},
    });
    scripts(7, false);
    const sandbox = attributes.get('sandbox').split(' ');
    assert.ok(!sandbox.includes('allow-scripts'));
    assert.ok(sandbox.includes('allow-same-origin') && sandbox.includes('allow-forms'));
    scripts(7, true);
    assert.equal(attributes.has('sandbox'), false);
});
