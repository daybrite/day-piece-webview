// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';
import { runInNewContext } from 'node:vm';
import { test } from 'node:test';

// Execute the shipped lifecycle code. Only the SDK import and export declarations are removed;
// the actual Web component is compile-checked by hvigor and exercised in HarmonyOS CI.
const source = readFileSync(new URL('../platform/harmony/ets/Controller.ets', import.meta.url), 'utf8')
  .replace(/^import .*;\n/gm, '').replace(/^export /gm, '');
const { Controller, evalPayload } = runInNewContext(
  `${stripTypeScriptTypes(source)}\n({ Controller, evalPayload })`, { console },
);

function setup(runJavaScript = () => Promise.resolve('1\x1fvalue')) {
  const commands = [];
  const replies = [];
  const controller = new Controller({
    runJavaScript,
    loadUrl: url => commands.push(['load', url]),
    refresh: () => commands.push(['reload']),
    stop: () => commands.push(['stop']),
    accessBackward: () => false,
    accessForward: () => true,
    backward: () => commands.push(['back']),
    forward: () => commands.push(['forward']),
  }, (request, payload) => replies.push([request, payload]));
  return { controller, commands, replies };
}

const drain = () => new Promise(resolve => setImmediate(resolve));

test('commands arriving before attachment run once, in order, when attached', () => {
  const s = setup();
  s.controller.command('load', 'resource://rawfile/day/site/next.html');
  s.controller.command('reload', '');
  assert.deepEqual(s.commands, []);
  s.controller.attach();
  s.controller.attach();
  s.controller.command('back', '');
  s.controller.command('forward', '');
  assert.deepEqual(s.commands, [
    ['load', 'resource://rawfile/day/site/next.html'], ['reload'], ['forward'],
  ]);
});

test('evaluation before attachment answers an error and works after attachment', async () => {
  const s = setup();
  s.controller.evaluate(1, 'document.title');
  assert.match(s.replies[0][1], /controller not attached/);
  s.controller.attach();
  s.controller.evaluate(2, 'document.title');
  await drain();
  assert.deepEqual(s.replies[1], [2, '1\x1fvalue']);
});

test('raw and JSON-quoted evaluation payloads preserve separators and Unicode', () => {
  const payload = '1\x1f"café 🌅"';
  assert.equal(evalPayload(payload), payload);
  assert.equal(evalPayload(JSON.stringify(payload)), payload);
  for (const invalid of ['', 'null', '"unterminated']) {
    assert.match(evalPayload(invalid), /^0\x1fArkWeb\x1f/);
  }
});

test('both synchronous controller errors and rejected promises answer exactly once', async () => {
  for (const evaluate of [
    () => { throw new Error('not attached'); },
    () => Promise.reject(new Error('renderer lost')),
  ]) {
    const s = setup(evaluate);
    s.controller.attach();
    s.controller.evaluate(3, 'document.title');
    await drain();
    assert.equal(s.replies.length, 1);
    assert.match(s.replies[0][1], /^0\x1fArkWeb\x1f/);
  }
});

test('disposal settles pending evaluations and ignores late engine replies', async () => {
  let resolve;
  const s = setup(() => new Promise(done => { resolve = done; }));
  s.controller.attach();
  s.controller.evaluate(4, 'document.title');
  s.controller.dispose();
  s.controller.dispose();
  resolve('1\x1flate');
  await drain();
  assert.deepEqual(s.replies, [[4, '0\x1fArkWeb\x1fview disposed']]);
});

test('a delayed attachment cannot revive a disposed view or flush its queued commands', () => {
  const s = setup();
  s.controller.command('load', 'https://example.test/');
  s.controller.dispose();
  s.controller.attach();
  s.controller.command('reload', '');
  s.controller.evaluate(5, 'document.title');
  assert.deepEqual(s.commands, []);
  assert.deepEqual(s.replies, [[5, '0\x1fArkWeb\x1fview disposed']]);
});

test('renderer exit settles every pending evaluation and rejects later evaluations', async () => {
  const resolvers = [];
  const s = setup(() => new Promise(done => resolvers.push(done)));
  s.controller.attach();
  s.controller.evaluate(6, 'one');
  s.controller.evaluate(7, 'two');
  s.controller.fail('renderer exited');
  s.controller.evaluate(8, 'three');
  for (const resolve of resolvers) resolve('1\x1flate');
  await drain();
  assert.deepEqual(s.replies, [6, 7, 8].map(id => [id, '0\x1fArkWeb\x1frenderer exited']));
});
