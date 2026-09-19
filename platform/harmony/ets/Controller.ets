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
      this.native.runJavaScript(script).then((result: string) => {
        this.finish(request, evalPayload(result));
      }).catch((err: Error) => {
        this.finish(request, evalError(String(err.message)));
      });
    } catch (err) {
      this.finish(request, evalError(String(err)));
    }
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
