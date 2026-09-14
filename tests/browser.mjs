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
