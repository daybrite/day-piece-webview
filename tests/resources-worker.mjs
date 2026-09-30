// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {runInNewContext} from 'node:vm';
import {MessageChannel} from 'node:worker_threads';
const source=readFileSync(new URL('../web/resource-worker.js',import.meta.url),'utf8');
function setup(reply, fetch=()=>{throw new Error('unexpected network request')}) {
  const events={};const requests=[];
  const self={location:new URL('https://example.test/app/assets/data/day-piece-webview/resource-worker.js'),addEventListener:(name,f)=>events[name]=f,
    clients:{matchAll:async()=>reply?[{postMessage:(request,[port])=>{requests.push(request);port.postMessage(reply);port.close();}}]:[]}};
  runInNewContext(source,{self,URL,Headers,Response,MessageChannel,setTimeout,clearTimeout,fetch});
  function load(path='session/1/pages/index.html',method='GET',headers={}) {
    let response;events.fetch({request:new Request(new URL(path,self.location),{method,headers}),respondWith:p=>response=p});return response;
  }
  return {load,requests};
}
test('binary provider responses preserve bytes, MIME and isolated iframe compatibility',async()=>{
  const s=setup({status:200,headers:[['Content-Type','image/png']],body:new Uint8Array([0,255,7])});
  const response=await s.load();assert.equal(response.status,200);
  assert.equal(response.headers.get('Cross-Origin-Embedder-Policy'),'require-corp');
  assert.equal(response.headers.get('Content-Type'),'image/png');
  assert.deepEqual([...new Uint8Array(await response.arrayBuffer())],[0,255,7]);
  assert.equal(s.requests[0].provider,1);assert.equal(s.requests[0].namespace,'session');
});
test('bundled assets are served at their virtual URL, including HEAD',async()=>{
  for(const method of ['GET','HEAD']) {
    const s=setup({status:200,headers:[['X-Day-Bundled-Asset','reader/index.html']],body:new Uint8Array()},async(url,options)=>{
      assert.equal(url.href,'https://example.test/app/assets/data/reader/index.html');
      assert.equal(options,undefined);
      return new Response('fixture',{headers:{'Content-Type':'text/html'}});
    });
    const r=await s.load('session/1/__day_assets/index.html',method);
    assert.equal(r.url,'');assert.equal(await r.text(),method==='HEAD'?'':'fixture');
  }
});
test('empty responses, absent owners and unrelated URLs finish predictably',async()=>{
  assert.equal((await setup({status:204,headers:[],body:new Uint8Array()}).load()).status,204);
  assert.equal((await setup(null).load()).status,410);
  assert.equal(setup(null).load('../reader/index.html'),undefined);
  assert.equal((await setup({status:200,headers:[['X-Day-Bundled-Asset','../../private']],body:[]}).load()).status,403);
});

test('bundled binary ranges and HEAD preserve lengths',async()=>{
  const s=setup({status:200,headers:[['X-Day-Bundled-Asset','site/a.bin']],body:[]},async()=>new Response(new Uint8Array([0,1,2,255])));
  let r=await s.load('session/1/__day_assets/a.bin','GET',{Range:'bytes=1-2'});
  assert.equal(r.status,206);assert.equal(r.headers.get('Content-Range'),'bytes 1-2/4');
  assert.deepEqual([...new Uint8Array(await r.arrayBuffer())],[1,2]);
  r=await s.load('session/1/__day_assets/a.bin','HEAD');
  assert.equal(r.headers.get('Content-Length'),'4');assert.equal(await r.text(),'');
  r=await s.load('session/1/__day_assets/a.bin','GET',{Range:'bytes=-0'});assert.equal(r.status,416);
});
