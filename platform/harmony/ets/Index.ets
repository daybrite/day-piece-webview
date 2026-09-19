// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0

// day-piece-webview: the HarmonyOS half of the piece (docs/extending.md, docs/webview.md).
//
// The ArkUI C node API has no Web node kind (native_node.h stops at the container types), so the
// component is declared here in ArkTS, built into a BuilderNode, and handed to Day as a FrameNode
// the native tree mounts like any other node. `day build` stages this file into the app's hvigor
// project and generates the aggregator that registers `dayPiece` with the shim.
//
// Commands arrive as the piece's own vocabulary ('load' | 'back' | 'forward' | 'stop' | 'reload')
// and are applied through the ArkTS WebviewController. Navigation reports the current URL back
// through `pieceEvent`, which reaches the piece's Rust front-end as an `Event::Custom`, so a
// bound text field follows along exactly as it does on the other backends.

import { BuilderNode, FrameNode, UIContext } from '@kit.ArkUI';
import { webview } from '@kit.ArkWeb';
import nativeEntry from 'libentry.so';
import { DayPieceModule } from '../DayPiece';
import { Controller, evalError } from './Controller';

// One live web view: its BuilderNode (which owns the component) and the controller commands go
// through. Held until Day disposes the node; a Web engine instance is expensive and does not go
// away with its FrameNode.
class DayWebViewInstance {
  node: BuilderNode<[DayWebViewParams]>;
  controller: Controller;

  constructor(node: BuilderNode<[DayWebViewParams]>, controller: Controller) {
    this.node = node;
    this.controller = controller;
  }
}

class DayWebViewParams {
  id: number = 0;
  url: string = '';
  // Inline mode (docs/webview.md): the URL prefix of the bundled site under rawfile ('' =
  // remote mode). A main-document navigation outside it is blocked in onLoadIntercept and
  // reported with num -1; the piece's Rust front-end runs the app's LinkPolicy.
  inlinePrefix: string = '';
  controller: webview.WebviewController;
  state: Controller;
  constructor(controller: webview.WebviewController, state: Controller) {
    this.controller = controller;
    this.state = state;
  }
}

// The component. `onPageEnd` fires for every committed navigation, including in-page link taps,
// which is what keeps the URL bar current. `onLoadIntercept` sees each main-document request
// before it loads, which is where an inline site's link policy runs (subresources go through a
// different callback and are never policed).
@Builder
function dayWebViewBuilder(params: DayWebViewParams) {
  // No percentage sizing: a BuilderNode is built detached, so '100%' resolves against the whole
  // window and the Web would cover the page. Day owns layout and sets this node's position + size
  // through the ArkUI C API like every other leaf, which is what the piece's `fill_measure` asks
  // for on every other backend too.
  Web({ src: params.url, controller: params.controller })
    .javaScriptAccess(true)
    .domStorageAccess(true)
    .onControllerAttached(() => {
      console.info(`day-piece-webview: controller attached for ${params.id}`);
      setTimeout(() => params.state.attach(), 0);
    })
    .onPageEnd((event) => {
      if (event !== undefined && !params.state.isDisposed()) {
        console.info(`day-piece-webview: page loaded: ${event.url}`);
        nativeEntry.pieceEvent(params.id, event.url);
      }
    })
    .onLoadIntercept((event) => {
      if (event === undefined || params.state.isDisposed() ||
        params.inlinePrefix === '' || !event.data.isMainFrame()) {
        return false; // remote mode and subframes do not dispatch app navigation
      }
      const url: string = event.data.getRequestUrl();
      if (url.startsWith(params.inlinePrefix) || url === 'about:blank') {
        return false; // in-site: let the engine navigate
      }
      // Leaving the site: block and report (num -1 = the link report); the Rust side runs the
      // app's LinkPolicy, system browser by default.
      nativeEntry.pieceEvent(params.id, url, -1);
      return true;
    })
    .onErrorReceive((event) => {
      if (event !== undefined) {
        console.error(`day-piece-webview: load failed: ${event.request.getRequestUrl()}: ` +
          `${event.error.getErrorCode()} ${event.error.getErrorInfo()}`);
      }
    })
    .onRenderExited((event) => {
      const reason: string = `renderer exited: ${event?.renderExitReason}`;
      console.error(`day-piece-webview: ${reason}`);
      params.state.fail(reason);
    })
}

const instances: Map<number, DayWebViewInstance> = new Map();

function make(ui: UIContext, id: number, props: string): FrameNode | undefined {
  // Decline where the system has no web engine rather than mount a component that cannot render.
  // A `Web` whose engine fails to load still creates its RosenWeb surface, and that surface wedges
  // the window's compositor for the rest of the process: every later frame goes stale, which is
  // far worse than Day's placeholder leaf. Declining reports the placeholder instead, exactly as
  // the GTK arm does where WebKitGTK is absent.
  if (!canIUse('SystemCapability.Web.Webview.Core')) {
    console.warn('day-piece-webview: no SystemCapability.Web.Webview.Core — rendering a placeholder');
    return undefined;
  }
  // The capability bit alone does not prove the engine's native libraries can load.
  // Initialize before constructing a Web surface and preserve loader diagnostics in hilog.
  if (!initializeEngine()) {
    return undefined;
  }
  const controller: webview.WebviewController = new webview.WebviewController();
  const state: Controller = new Controller(controller, (request: number, payload: string) => {
    nativeEntry.pieceEvent(id, payload, request);
  });
  const params: DayWebViewParams = new DayWebViewParams(controller, state);
  params.id = id;
  // Inline mode arrives as `day-inline<US><root>[<US><start>]` (0x1F-separated, the piece's own
  // props encoding): the site lives under rawfile `day/<root>/` (resources/arkui.rs staging),
  // which ArkWeb browses through `resource://rawfile/` URLs, so relative references resolve
  // within it natively.
  const SEP: string = '\u001F';
  if (props.startsWith('day-inline' + SEP)) {
    const parts: string[] = props.split(SEP);
    const root: string = parts.length > 1 ? parts[1] : '';
    const start: string = parts.length > 2 && parts[2] !== '' ? parts[2] : 'index.html';
    params.inlinePrefix = 'resource://rawfile/day/' + root + '/';
    params.url = params.inlinePrefix + start;
  } else {
    params.url = props;
  }
  const node: BuilderNode<[DayWebViewParams]> = new BuilderNode(ui);
  try {
    node.build(wrapBuilder<[DayWebViewParams]>(dayWebViewBuilder), params);
    const frame: FrameNode | null = node.getFrameNode();
    if (frame !== null) {
      instances.set(id, new DayWebViewInstance(node, state));
      return frame;
    }
  } catch (err) {
    console.error(`day-piece-webview: component creation failed: ${err}`);
  }
  state.dispose();
  node.dispose();
  return undefined;
}

let engineInitialized: boolean = false;
function initializeEngine(): boolean {
  if (engineInitialized) {
    return true;
  }
  try {
    webview.WebviewController.initializeWebEngine();
    console.info('day-piece-webview: engine initialization requested');
    engineInitialized = true;
    return true;
  } catch (err) {
    console.error(`day-piece-webview: engine initialization failed: ${err}`);
    return false;
  }
}

function update(id: number, cmd: string, arg: string): void {
  const live: DayWebViewInstance | undefined = instances.get(id);
  if (cmd === 'eval') {
    const separator: number = arg.indexOf('\u001F');
    const request: number = Number(arg.substring(0, separator));
    if (live === undefined) {
      // A declined/failed builder must answer too; otherwise the Rust future never resolves.
      nativeEntry.pieceEvent(id, evalError('no live web view'), request);
    } else {
      live.controller.evaluate(request, arg.substring(separator + 1));
    }
    return;
  }
  if (live !== undefined) {
    live.controller.command(cmd, arg);
  }
}

function dispose(id: number): void {
  const live: DayWebViewInstance | undefined = instances.get(id);
  if (live !== undefined) {
    instances.delete(id);
    live.controller.dispose();
    live.node.dispose();
  }
}

export const dayPiece: DayPieceModule = {
  kind: 'day.piece.webview',
  make: make,
  update: update,
  dispose: dispose
};
