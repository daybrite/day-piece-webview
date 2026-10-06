// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

import { webview } from '@kit.ArkWeb';

export function evalError(message: string): string {
  return '0\u001FArkWeb\u001F' + message;
}

// The Rust wrapper always returns a string. ArkWeb versions can return that string raw or
// JSON-quoted; unwrap only the latter, preserving Unicode and the protocol separators.
export function evalPayload(result: string): string {
  if (result === '' || result === 'null') {
    return evalError('no result');
  }
  if (result.startsWith('"')) {
    try {
      const value: Object | null = JSON.parse(result);
      return typeof value === 'string' ? value : evalError('non-string result');
    } catch (err) {
      return evalError('unparsable result');
    }
  }
  return result;
}

class DataRequest {
  operation: string = '';
  url: string = '';
  cookie: string = '';
}

class Command {
  name: string;
  arg: string;
  constructor(name: string, arg: string) {
    this.name = name;
    this.arg = arg;
  }
}

// Kept separate from the ArkUI builder so lifecycle behavior can be tested without an emulator.
export class Controller {
  private native: webview.WebviewController;
  private reply: (request: number, payload: string) => void;
  private privateMode: boolean = false;
  private loading: boolean = false;
  setLoading(value: boolean): void { this.loading = value; }
  setPrivate(value: boolean): void { this.privateMode = value; }
  private ready: boolean = false;
  private disposed: boolean = false;
  private failure: string = '';
  private commands: Command[] = [];
  private pending: Set<number> = new Set();

  constructor(native: webview.WebviewController, reply: (request: number, payload: string) => void) {
    this.native = native;
    this.reply = reply;
  }

  isDisposed(): boolean {
    return this.disposed;
  }

  // Called on the next UI turn after onControllerAttached: the attachment callback itself
  // must not manipulate pages. A view disposed before that turn must never be revived.
  attach(): void {
    if (this.disposed || this.failure !== '') {
      return;
    }
    this.ready = true;
    const queued: Command[] = this.commands;
    this.commands = [];
    queued.forEach((command: Command) => this.command(command.name, command.arg));
  }

  command(name: string, arg: string): void {
    if (this.disposed || this.failure !== '') {
      return;
    }
    if (!this.ready) {
      this.commands.push(new Command(name, arg));
      return;
    }
    try {
      if (name === 'load') {
        this.native.loadUrl(arg);
      } else if (name === 'back' && this.native.accessBackward()) {
        this.native.backward();
      } else if (name === 'forward' && this.native.accessForward()) {
        this.native.forward();
      } else if (name === 'stop') {
        this.native.stop();
      } else if (name === 'forceReload') {
        // ArkWeb exposes cache eviction, not a per-navigation cache bypass.
        this.native.removeCache(true);
        this.native.refresh();
      } else if (name === 'reload') {
        this.native.refresh();
      }
    } catch (err) {
      console.error(`day-piece-webview: ${name} failed: ${err}`);
    }
  }

  evaluate(request: number, script: string): void {
    if (this.disposed || this.failure !== '' || !this.ready) {
      const reason: string = this.disposed ? 'view disposed' :
        (this.failure !== '' ? this.failure : 'controller not attached');
      this.reply(request, evalError(reason));
      return;
    }
    this.pending.add(request);
    try {
      if (script.startsWith('day-web-data:')) {
        const data: DataRequest = JSON.parse(script.substring(13)) as DataRequest;
        if(data.operation === 'navigation') {
          this.finish(request, '1\u001F' + JSON.stringify({url:this.native.getUrl(),title:this.native.getTitle(),can_go_back:this.native.accessBackward(),can_go_forward:this.native.accessForward(),loading:this.loading}));
        } else if(data.operation === 'cookies') {
          const cookies: string = webview.WebCookieManager.fetchCookieSync(data.url, this.privateMode);
          this.finish(request, '1\u001F' + JSON.stringify(cookies));
        } else if(data.operation === 'set-cookie') {
          webview.WebCookieManager.configCookieSync(data.url, data.cookie, this.privateMode, true);
          this.finish(request, '1\u001Ftrue');
        } else {
          // ArkWeb cannot delete one profile's complete site data. Never erase other sites.
          this.finish(request, evalError('Complete profile clearing is unsupported by ArkWeb'));
        }
        return;
      }
      this.native.runJavaScript(script).then((result: string) => {
        this.finish(request, evalPayload(result));
      }).catch((err: Error) => {
        this.finish(request, evalError(String(err.message)));
      });
    } catch (err) {
      this.finish(request, evalError(String(err)));
    }
  }

  // Before a dayscript screenshot (`settle` in Index.ets): resolve once the page has committed
  // a renderer frame holding its current DOM. ArkWeb paints on its own thread, trailing the
  // DOM a script just changed by frames (seconds under the emulator's software GL), and
  // ArkUI's render checkpoint never sees that work. Four animation frames in the page put the
  // commit through the compositor's pipeline (a frame is on screen two or three begin-frames
  // after the one that drew it, and under software GL each of those is long), and a CSS
  // transition the change started (a toolbar sliding away) runs out first: finite, unlike an
  // animation, which a capture never waits for. A page that paints none (hidden, mid-load) runs
  // out the cap instead, so a capture is late at worst, never stuck. A view with no page yet
  // has nothing to wait for.
  settle(cap: number = 2000): Promise<void> {
    if (this.disposed || this.failure !== '' || !this.ready) {
      return Promise.resolve();
    }
    const arm: string = '(()=>{const w=window;w.__daySettled=w.__daySettled||0;' +
      'const t=(w.__daySettle=(w.__daySettle||0)+1);' +
      'let n=4;const done=()=>{const busy=(document.getAnimations?document.getAnimations():[])' +
      '.some(a=>a instanceof CSSTransition&&a.playState==="running");' +
      'if(busy||--n>0)requestAnimationFrame(done);else w.__daySettled=t};' +
      'requestAnimationFrame(done);return String(t)})()';
    const deadline: number = Date.now() + cap;
    return this.native.runJavaScript(arm).then((raw: string): Promise<void> => {
      const token: string = evalPayload(raw);
      return this.settled(token, deadline);
    }).catch((): void => {});
  }

  private settled(token: string, deadline: number): Promise<void> {
    return this.native.runJavaScript('String(window.__daySettled)').then((raw: string): Promise<void> => {
      if (evalPayload(raw) === token || Date.now() >= deadline || this.disposed) {
        return Promise.resolve();
      }
      return new Promise<void>((resolve: () => void) => setTimeout(resolve, 16))
        .then((): Promise<void> => this.settled(token, deadline));
    });
  }

  private finish(request: number, payload: string): void {
    // Disposing a node or losing its renderer may already have answered this request.
    if (this.pending.delete(request)) {
      this.reply(request, payload);
    }
  }

  fail(reason: string): void {
    this.failure = reason;
    this.ready = false;
    this.commands = [];
    this.pending.forEach((request: number) => this.finish(request, evalError(reason)));
  }

  dispose(): void {
    if (!this.disposed) {
      this.disposed = true;
      this.fail('view disposed');
    }
  }
}
