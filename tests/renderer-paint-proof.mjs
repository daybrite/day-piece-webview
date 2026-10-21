// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { createContext, runInContext } from 'node:vm';

// Execute the actual proof expressions used by the comparison walkthrough. No browser
// dependency: control document loading and frame callbacks to reproduce the CI race.
const walkthrough = readFileSync(new URL('../demo/experiments/renderer-comparison.yaml', import.meta.url), 'utf8');
const proofs = [...walkthrough.matchAll(/      script: \|\n((?:        .*\n)+)/g)]
  .map(match => match[1].replace(/^        /gm, ''));
assert.equal(proofs.length, 2, 'both native and Servo proofs must use the readiness gate');

for (const [index, script] of proofs.entries()) {
  function fixture() {
    const frames = [];
    const document = {
      readyState: 'loading',
      documentElement: { dataset: {} },
      body: { style: {} },
    };
    let stylesheet = '';
    const context = createContext({
      document, window: {},
      getComputedStyle: () => ({ getPropertyValue: () => stylesheet }),
      requestAnimationFrame: callback => frames.push(callback),
    });
    return { document, frames, context,
      poll: () => runInContext(script, context),
      loadStyles: () => { stylesheet = 'yes'; },
    };
  }

  test(`proof ${index}: a complete blank document or unfinished resources cannot pass`, () => {
    const f = fixture();
    // An initial about:blank document can report complete and answer JavaScript.
    f.document.readyState = 'complete';
    assert.equal(f.poll(), 'loading');
    f.document.documentElement.dataset.resourceScript = 'yes';
    assert.equal(f.poll(), 'loading');
    f.loadStyles();
    f.document.readyState = 'interactive';
    assert.equal(f.poll(), 'loading');
    assert.deepEqual(f.document.body.style, {});
    assert.equal(f.frames.length, 0);
    f.document.readyState = 'complete';
    assert.equal(f.poll(), 'pending');
    assert.equal(f.document.body.style.background, '#00ff00');
  });

  test(`proof ${index}: retries wait for both frames without restarting the mutation`, () => {
    const f = fixture();
    f.document.readyState = 'complete';
    f.document.documentElement.dataset.resourceScript = 'yes';
    f.loadStyles();
    assert.equal(f.poll(), 'pending');
    assert.equal(f.poll(), 'pending');
    assert.equal(f.frames.length, 1);
    f.frames.shift()();
    assert.equal(f.poll(), 'pending');
    assert.equal(f.frames.length, 1);
    f.frames.shift()();
    assert.equal(f.poll(), 'painted');
    assert.equal(f.frames.length, 0);
    assert.equal(f.poll(), 'painted');
  });
}
