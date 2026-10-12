// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { rendererComparisons } from '../demo/website/components/renderer-comparisons.mjs';

function shot(engine, target = 'macos-appkit', variant = 'default', prefix = 'main/') {
  return {
    id: `renderer-bundled-${engine}`,
    title: { en: engine, fr: `fixture-${engine}` }, caption: { en: 'fixture page' },
    byPlatform: { [target]: { [variant]: { src: `${prefix}gallery/${target}/${variant}/${engine}.png` } } },
  };
}

test('comparison preserves channel URLs and localized labels, with native first', () => {
  const pairs = rendererComparisons({ shots: [shot('servo'), shot('webview')] }, 'fr');
  assert.equal(pairs.length, 1);
  assert.deepEqual(pairs[0].captures.map((capture) => capture.title), ['fixture-webview', 'fixture-servo']);
  assert.ok(pairs[0].captures.every((capture) => capture.src.startsWith('main/gallery/')));
  assert.equal(rendererComparisons({ shots: [] }, 'en').length, 0);
});

test('missing engines, different targets, and different variants never form a pair', () => {
  for (const shots of [
    [shot('webview')],
    [shot('webview'), shot('servo', 'linux-gtk')],
    [shot('webview'), shot('servo', 'macos-appkit', 'dark')],
  ]) assert.deepEqual(rendererComparisons({ shots }, 'en'), []);
});


test('complete platform pairs remain distinct and name the target', () => {
  const native = shot('webview');
  const servo = shot('servo');
  Object.assign(native.byPlatform, shot('webview', 'android-mdc').byPlatform);
  Object.assign(servo.byPlatform, shot('servo', 'android-mdc').byPlatform);
  const pairs = rendererComparisons({ shots: [native, servo] }, 'en');
  assert.equal(pairs.length, 2);
  assert.deepEqual(pairs.map(pair => pair.title), ['fixture page · macos-appkit', 'fixture page · android-mdc']);
  for (const pair of pairs) assert.ok(pair.captures.every(capture => capture.src.includes(pair.id.split('-bundled-')[0])));
});
