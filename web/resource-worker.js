// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
// This worker controls only the piece's resource directory, not the app's pages or PWA.
self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
self.addEventListener('activate', event => event.waitUntil(self.clients.claim()));

function response(body, status, sourceHeaders = []) {
  const headers = new Headers(sourceHeaders);
  // Day's isolated host needs this on child documents too, including error documents.
  headers.set('Cross-Origin-Embedder-Policy', 'require-corp');
  headers.set('Cache-Control', 'no-store');
  return new Response(body, {status, headers});
}

async function bundled(asset, request) {
  const target = new URL('../' + asset, self.location);
  const root = new URL('../', self.location);
  if (target.origin !== root.origin || !target.pathname.startsWith(root.pathname)) return response(null, 403);
  // Fetch outside our reserved directory. Reconstruct the response so its bundle URL
  // does not replace the virtual URL used to resolve document-relative references.
  const file = await fetch(target);
  const headers = new Headers(file.headers);
  headers.delete('Content-Encoding');
  headers.delete('Content-Length');
  const range = request.headers.get('Range');
  if (file.status === 200 && (range || request.method === 'HEAD')) {
    const body = await file.arrayBuffer();
    const len = body.byteLength;
    headers.set('Content-Length', String(len));
    headers.set('Accept-Ranges', 'bytes');
    if (request.method === 'HEAD') return response(null, 200, headers);
    const match = /^bytes=(\d*)-(\d*)$/.exec(range);
    let first, last;
    if (match && (match[1] || match[2])) {
      first = match[1] ? Number(match[1]) : Math.max(0, len - Number(match[2]));
      last = match[1] && match[2] ? Math.min(len - 1, Number(match[2])) : len - 1;
    }
    if (!Number.isSafeInteger(first) || !Number.isSafeInteger(last) || first > last || first >= len) {
      headers.set('Content-Range', `bytes */${len}`);
      headers.set('Content-Length', '0');
      return response(null, 416, headers);
    }
    headers.set('Content-Range', `bytes ${first}-${last}/${len}`);
    headers.set('Content-Length', String(last - first + 1));
    return response(body.slice(first, last + 1), 206, headers);
  }
  return response(request.method === 'HEAD' ? null : file.body, file.status, headers);
}

self.addEventListener('fetch', event => {
  const root = new URL('./', self.location).pathname;
  const url = new URL(event.request.url);
  if (url.origin !== self.location.origin || !url.pathname.startsWith(root)) return;
  const [namespace, provider] = url.pathname.slice(root.length).split('/');
  if (!namespace || !/^\d+$/.test(provider)) return;
  event.respondWith((async () => {
    const clients = await self.clients.matchAll({type: 'window', includeUncontrolled: true});
    return new Promise(resolve => {
      const channels = [];
      let ended = false;
      const finish = result => {
        if (ended) return;
        ended = true;
        clearTimeout(timer);
        channels.forEach(port => port.close());
        resolve(result);
      };
      const timer = setTimeout(() => finish(response(null, 410)), 30000);
      for (const client of clients) {
        const channel = new MessageChannel();
        channels.push(channel.port1);
        channel.port1.onmessage = async message => {
          const r = message.data;
          if (!r?.status || ended) return;
          try {
            const headers = new Headers(r.headers);
            const asset = headers.get('X-Day-Bundled-Asset');
            if (asset) finish(await bundled(asset, event.request));
            else finish(response(event.request.method === 'HEAD' || r.status === 204 || r.status === 205 ? null : r.body, r.status, headers));
          } catch { finish(response(null, 500)); }
        };
        client.postMessage({type: 'day-resource', namespace, provider: Number(provider), url: url.href,
          method: event.request.method, range: event.request.headers.get('Range') || ''}, [channel.port2]);
      }
      if (!clients.length) finish(response(null, 410));
    });
  })());
});
