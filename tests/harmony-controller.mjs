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
const cookieCalls = [];
const webview = { WebCookieManager: {
  fetchCookieSync: (url, privateMode) => { cookieCalls.push(['read', url, privateMode]); return 'session=fixture'; },
  configCookieSync: (url, cookie, privateMode, httpOnly) => cookieCalls.push(['write', url, cookie, privateMode, httpOnly]),
} };
const { Controller, evalPayload, desktopAgent, messageReport } = runInNewContext(
  `${stripTypeScriptTypes(source)}\n({ Controller, evalPayload, desktopAgent, messageReport })`, { console, webview, setTimeout, clearTimeout },
);
const ARKWEB_AGENT = 'Mozilla/5.0 (Phone; OpenHarmony 5.0) AppleWebKit/537.36 (KHTML, like Gecko) ' +
  'Chrome/114.0.0.0 Safari/537.36 ArkWeb/4.1.6.1 Mobile';

function setup(runJavaScript = () => Promise.resolve('1\x1fvalue')) {
  const commands = [];
  const replies = [];
  const controller = new Controller({
    runJavaScript,
    getUrl: () => "https://fixture.example/second",
    getTitle: () => "Fixture page",
    loadUrl: url => commands.push(['load', url]),
    refresh: () => commands.push(['reload']),
    removeCache: disk => commands.push(['cache',disk]),
    stop: () => commands.push(['stop']),
    accessBackward: () => false,
    accessForward: () => true,
    backward: () => commands.push(['back']),
    forward: () => commands.push(['forward']),
    getUserAgent: () => ARKWEB_AGENT,
    setCustomUserAgent: agent => commands.push(['agent', agent]),
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


test('native cookie operations use the correct persistent or incognito jar', () => {
  for (const privateMode of [false, true]) {
    cookieCalls.length = 0;
    const s = setup(); s.controller.setPrivate(privateMode); s.controller.attach();
    s.controller.evaluate(21, 'day-web-data:' + JSON.stringify({operation:'cookies',url:'https://fixture.example/'}));
    s.controller.evaluate(22, 'day-web-data:' + JSON.stringify({operation:'set-cookie',url:'https://fixture.example/',cookie:'session=fixture; HttpOnly'}));
    assert.deepEqual(cookieCalls, [
      ['read','https://fixture.example/',privateMode],
      ['write','https://fixture.example/','session=fixture; HttpOnly',privateMode,true],
    ]);
    assert.deepEqual(s.replies, [[21,'1\x1f"session=fixture"'],[22,'1\x1ftrue']]);
  }
});

test('unsupported complete profile clearing never clears unrelated sites', () => {
  cookieCalls.length = 0;
  const s = setup(); s.controller.attach();
  s.controller.evaluate(23, 'day-web-data:' + JSON.stringify({operation:'clear'}));
  assert.equal(cookieCalls.length, 0);
  assert.match(s.replies[0][1], /Complete profile clearing is unsupported/);
});


test('navigation snapshots expose actual controller history and loading state', () => {
  const s=setup();s.controller.attach();s.controller.setLoading(true);
  // Page-state reports (-5) share the reply channel; this test reads the evaluation replies.
  const replies=()=>s.replies.filter(reply=>reply[0]!==-5);
  s.controller.evaluate(24,'day-web-data:'+JSON.stringify({operation:'navigation'}));
  const state=JSON.parse(replies()[0][1].substring(2));
  assert.deepEqual(state,{url:'https://fixture.example/second',title:'Fixture page',can_go_back:false,can_go_forward:true,loading:true});
  s.controller.setLoading(false);s.controller.evaluate(25,'day-web-data:'+JSON.stringify({operation:'navigation'}));
  assert.equal(JSON.parse(replies()[1][1].substring(2)).loading,false);
});


test('force reload evicts the ArkWeb resource cache before navigation, without touching cookies', () => {
  cookieCalls.length=0;
  const s=setup();s.controller.attach();s.controller.command('forceReload','');
  assert.deepEqual(s.commands,[['cache',true],['reload']]);
  assert.equal(cookieCalls.length,0);
});


test('a capture settles on the frame the page commits after its latest change', async () => {
  // The page answers the arm with a token and reports it settled on the third poll.
  let polls = 0;
  const scripts = [];
  const s = setup(script => {
    scripts.push(script);
    if (script.includes('requestAnimationFrame')) return Promise.resolve('"7"');
    polls += 1;
    return Promise.resolve(polls < 3 ? '6' : '7');
  });
  s.controller.attach();
  await s.controller.settle();
  assert.equal(polls, 3);
  assert.match(scripts[0], /let n=4;.*requestAnimationFrame\(done\)/);
});

test('a page that never commits a frame runs out the cap, and a bare view settles at once', async () => {
  let calls = 0;
  const s = setup(() => { calls += 1; return Promise.resolve(calls === 1 ? '3' : '2'); });
  await s.controller.settle(40); // not attached: nothing to wait for
  assert.equal(calls, 0);
  s.controller.attach();
  const start = Date.now();
  await s.controller.settle(40);
  assert.ok(Date.now() - start >= 30 && calls >= 2, `polled ${calls} times`);
  const failing = setup(() => Promise.reject(new Error('renderer lost')));
  failing.controller.attach();
  await failing.controller.settle(40); // a lost renderer never blocks the capture
});

test('an engine that never answers runs out the cap rather than holding the capture', async () => {
  // A renderer stuck in page script leaves every runJavaScript pending forever: measured on the
  // OpenHarmony emulator, where lottie-web swapping in an animation wedges ArkWeb's renderer.
  const stuck = setup(() => new Promise(() => {}));
  stuck.controller.attach();
  const start = Date.now();
  await stuck.controller.settle(40);
  assert.ok(Date.now() - start >= 30, 'resolved before the cap');
  // Arming answers, then the polls never do.
  let calls = 0;
  const stalled = setup(() => (calls++ === 0 ? Promise.resolve('"1"') : new Promise(() => {})));
  stalled.controller.attach();
  await stalled.controller.settle(40);
  assert.equal(calls, 2);
});

test('an engine that throws instead of answering never blocks the capture', async () => {
  const s = setup(() => { throw new Error('17100001 Init error'); });
  s.controller.attach();
  await s.controller.settle(40);
});

test('page state reports carry history, progress and title, and skip repeats', () => {
  const s = setup();
  s.controller.setLoading(true);
  assert.deepEqual(s.replies, [], 'nothing reports before the controller is attached');
  s.controller.attach();
  s.controller.setLoading(true);
  s.controller.setProgress(42);
  s.controller.setProgress(42);
  s.controller.report();
  s.controller.setLoading(false);
  assert.deepEqual(s.replies, [
    [-5, '101\x1f0.00\x1fhttps://fixture.example/second\x1fFixture page'],
    [-5, '101\x1f0.42\x1fhttps://fixture.example/second\x1fFixture page'],
    [-5, '001\x1f1\x1fhttps://fixture.example/second\x1fFixture page'],
  ]);
});

test('snapshots report unsupported in the shape the front-end maps', () => {
  const s = setup();
  s.controller.attach();
  s.controller.evaluate(7, 'day-web-data:{"operation":"snapshot","width":200}');
  assert.equal(s.replies.length, 1);
  assert.equal(s.replies[0][0], 7);
  assert.match(s.replies[0][1], /^0\x1fUnsupported\x1f/);
});

test('desktop mode sets a desktop Chrome agent at the engine version, and restores the default', () => {
  const desktop = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) ' +
    'Chrome/114.0.0.0 Safari/537.36';
  assert.equal(desktopAgent(ARKWEB_AGENT), desktop);
  assert.match(desktopAgent('ArkWeb/4.1 Mobile'), /Chrome\/120\.0\.0\.0 /);
  // A view created in desktop mode queues the agent ahead of its first page (Index.ets).
  const s = setup();
  s.controller.command('desktop', '1');
  s.controller.command('load', 'https://fixture.example/');
  s.controller.attach();
  s.controller.command('desktop', '0');
  assert.deepEqual(s.commands, [
    ['agent', desktop], ['load', 'https://fixture.example/'], ['agent', ARKWEB_AGENT],
  ]);
});

test('page message envelopes become the front-end report, and replies run in the page', async () => {
  const report = JSON.parse(messageReport(JSON.stringify({ dayMessage: 'dayAdblock', id: 41, payload: '{"type":"page"}' })));
  assert.deepEqual({ ...report }, { id: 41, name: 'dayAdblock', payload: '{"type":"page"}', origin: '', main_frame: false });
  assert.equal(messageReport('not json'), '');
  assert.equal(messageReport(JSON.stringify({ id: 1 })), '');
  const ran = [];
  const { controller } = setup((script) => { ran.push(script); return Promise.resolve(''); });
  controller.attach();
  controller.command('run', 'window.__dayMessageReply&&window.__dayMessageReply(41,null)');
  assert.deepEqual(ran, ['window.__dayMessageReply&&window.__dayMessageReply(41,null)']);
});
