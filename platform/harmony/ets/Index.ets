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
// and are applied through the ArkTS WebviewController; the settings ('zoom' | 'javascript' |
// 'desktop') through the component's attributes or the controller's user agent. Navigation
// reports the current URL back through `pieceEvent`, which reaches the piece's Rust front-end as
// an `Event::Custom`, so a bound text field follows along exactly as it does on the other
// backends.

import { BuilderNode, FrameNode, UIContext } from '@kit.ArkUI';
import { webview } from '@kit.ArkWeb';
import nativeEntry from 'libentry.so';
import { DayPieceModule } from '../DayPiece';
import { Controller, evalError, messageReport } from './Controller';

// One live web view: its BuilderNode (which owns the component) and the controller commands go
// through. Held until Day disposes the node; a Web engine instance is expensive and does not go
// away with its FrameNode.
class DayWebViewInstance {
  node: BuilderNode<[DayWebViewParams]>;
  controller: Controller;
  // What the builder was built from: a setting held in it changes by updating the node with it.
  params: DayWebViewParams;

  constructor(node: BuilderNode<[DayWebViewParams]>, controller: Controller, params: DayWebViewParams) {
    this.node = node;
    this.controller = controller;
    this.params = params;
  }
}

// The page side of WebView::on_script_message: the dayMessage shim posts its envelope here, and
// the report reaches the Rust front-end on num -8.
class DayMessageHost {
  id: number = 0;
  disposed: () => boolean = () => false;
  post(envelope: string): void {
    const report: string = messageReport(envelope);
    if (report !== '' && !this.disposed()) {
      nativeEntry.pieceEvent(this.id, report, -8);
    }
  }
}

class ContentProps {
  filter: boolean = false;
  messages: boolean = false;
  scripts: string = '';
}

class DayWebViewParams {
  id: number = 0;
  tabs: boolean = false;
  foregroundLabel: string = '';
  backgroundLabel: string = '';
  ui: UIContext | undefined = undefined;
  url: string = '';
  // Inline mode (docs/webview.md): the URL prefix of the bundled site under rawfile ('' =
  // remote mode). A main-document navigation outside it is blocked in onLoadIntercept and
  // reported with num -1; the piece's Rust front-end runs the app's LinkPolicy.
  inlinePrefix: string = '';
  // WebView::transparent: the component draws no background of its own, so a page whose own
  // background is transparent shows the app behind it.
  transparent: boolean = false;
  resources: boolean = false;
  // The first page waits for the controller (src stays about:blank): a desktop agent can only
  // be set through it, and the first load must already carry it.
  deferLoad: boolean = false;
  // WebView::zoom (as ArkWeb's text zoom percentage) and WebView::javascript.
  textZoom: number = 100;
  scripts: boolean = true;
  privateMode: boolean = false;
  // Content blocking and page scripts (docs/webview.md): a request filter waits for the native
  // scheme handler before the first page, like a provider; `startScripts` is one document-start
  // script (the app's, in order); `messages` names the page message handlers, reached through
  // the `__dayMessageHost` proxy.
  filter: boolean = false;
  startScripts: string = '';
  messages: boolean = false;
  messageHost: DayMessageHost = new DayMessageHost();
  controller: webview.WebviewController;
  state: Controller;
  constructor(controller: webview.WebviewController, state: Controller) {
    this.controller = controller;
    this.state = state;
  }
}

const TAB_MODIFIER_SCRIPT: string = "(()=>{const click=e=>{const a=e.composedPath().find(n=>n?.tagName==='A'&&n.href);if(!a||!(e.metaKey||e.ctrlKey||e.button===1))return;e.preventDefault();e.stopImmediatePropagation();location.href='day-tab://open/'+(e.shiftKey?'0':'1')+'/'+encodeURIComponent(a.href);};addEventListener('click',click,true);addEventListener('auxclick',click,true);})();";

// The document-start scripts: the tab modifier hook, then the app's page scripts as one script
// (the Rust front-end joins them in order, so ArkWeb's ordering of script items cannot reorder
// them).
function startScripts(params: DayWebViewParams): ScriptItem[] {
  const scripts: ScriptItem[] = [];
  if (params.tabs) {
    scripts.push({ script: TAB_MODIFIER_SCRIPT, scriptRules: ['*'] });
  }
  if (params.startScripts !== '') {
    scripts.push({ script: params.startScripts, scriptRules: ['*'] });
  }
  return scripts;
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
  Web({ src: params.resources || params.filter || params.deferLoad ? 'about:blank' : params.url, controller: params.controller, incognitoMode: params.privateMode })
    .backgroundColor(params.transparent ? Color.Transparent : Color.White)
    // Page scripts only: `runJavaScript` still runs the app's evaluation with them off.
    .javaScriptAccess(params.scripts)
    // Text zoom, like Android WebView's: it persists across navigations and reflows the text.
    .textZoomRatio(params.textZoom)
    .domStorageAccess(true)
    .multiWindowAccess(params.tabs)
    // The page message host (WebView::on_script_message); no methods without handlers.
    .javaScriptProxy({object: params.messageHost, name: '__dayMessageHost',
      methodList: params.messages ? ['post'] : [], controller: params.controller})
    .javaScriptOnDocumentStart(startScripts(params))
    .onWindowNew((event) => {
      if (params.tabs && event.isUserTrigger && event.targetUrl !== '') {
        nativeEntry.pieceEvent(params.id,JSON.stringify({url:event.targetUrl,background:false}),-4);
      }
    })
    .onContextMenuShow((event) => {
      const url: string = event.param.getLinkUrl();
      if (!params.tabs || url === '') return false;
      params.ui?.getPromptAction().showActionMenu({buttons:[
        {text:params.foregroundLabel,color:'#007DFF'},
        {text:params.backgroundLabel,color:'#007DFF'}
      ]}).then((result) => {
        if(!params.state.isDisposed())nativeEntry.pieceEvent(params.id,JSON.stringify({url:url,background:result.index===1}),-4);
      }).catch(() => {});
      return true;
    })
    .onControllerAttached(() => {
      console.info(`day-piece-webview: controller attached for ${params.id}`);
      setTimeout(() => {params.state.attach();if(params.resources||params.filter)nativeEntry.pieceEvent(params.id,'day-web-'+params.id,-3);}, 0);
    })
    .onPageBegin(() => { params.state.setLoading(true); })
    // Live page state (WebView::page_state): the controller re-reads and reports on each change.
    .onTitleReceive(() => { params.state.report(); })
    .onProgressChange((event) => {
      if (event !== undefined) {
        params.state.setProgress(event.newProgress);
      }
    })
    .onRefreshAccessedHistory(() => { params.state.report(); })
    .onPageEnd((event) => {
      params.state.setLoading(false);
      if (event !== undefined && !params.state.isDisposed()) {
        console.info(`day-piece-webview: page loaded: ${event.url}`);
        nativeEntry.pieceEvent(params.id, event.url);
      }
    })
    .onLoadIntercept((event) => {
      if (event !== undefined && params.tabs && !params.state.isDisposed()) {
        const request: string = event.data.getRequestUrl();
        if (request.startsWith('day-tab://open/')) {
          const parts: string[] = request.substring('day-tab://open/'.length).split('/');
          nativeEntry.pieceEvent(params.id,JSON.stringify({url:decodeURIComponent(parts.slice(1).join('/')),background:parts[0]==='1'}),-4);
          return true;
        }
      }
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
      params.state.setLoading(false);
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
  const controller: webview.WebviewController = new webview.WebviewController('day-web-'+id);
  const state: Controller = new Controller(controller, (request: number, payload: string) => {
    nativeEntry.pieceEvent(id, payload, request);
  });
  const params: DayWebViewParams = new DayWebViewParams(controller, state);
  params.id = id;
  params.ui = ui;
  // Inline mode arrives as `day-inline<US><root>[<US><start>]` (0x1F-separated, the piece's own
  // props encoding): the site lives under rawfile `day/<root>/` (resources/arkui.rs staging),
  // which ArkWeb browses through `resource://rawfile/` URLs, so relative references resolve
  // within it natively.
  const SEP: string = '\u001F';
  // A transparent view leads with its own marker (lib-arkui.rs), ahead of everything below.
  let rest: string = props;
  // Settings other than the defaults lead with theirs, outermost:
  // `day-settings<US><text zoom percent><US><desktop 0/1><US><scripts 0/1><US>`.
  // Content blocking and page scripts lead with theirs, outermost of all:
  // `day-content<US><JSON {filter, messages, scripts}><US>`. JSON escapes control characters,
  // so the separator cannot occur inside it.
  if (rest.startsWith('day-content' + SEP)) {
    const from: number = ('day-content' + SEP).length;
    const to: number = rest.indexOf(SEP, from);
    try {
      const content: ContentProps = JSON.parse(rest.substring(from, to)) as ContentProps;
      params.filter = content.filter === true;
      params.messages = content.messages === true;
      params.startScripts = typeof content.scripts === 'string' ? content.scripts : '';
    } catch (err) {
      console.error(`day-piece-webview: content props: ${err}`);
    }
    rest = rest.substring(to + 1);
  }
  params.messageHost.id = id;
  params.messageHost.disposed = () => state.isDisposed();
  let desktop: boolean = false;
  if (rest.startsWith('day-settings' + SEP)) {
    const settings: string[] = rest.split(SEP);
    params.textZoom = Number(settings[1]);
    desktop = settings[2] === '1';
    params.scripts = settings[3] !== '0';
    rest = settings.slice(4).join(SEP);
  }
  if (rest.startsWith('day-tabs'+SEP)) {
    const labels: string[] = rest.split(SEP);
    params.tabs=true;params.foregroundLabel=labels[1];params.backgroundLabel=labels[2];
    rest=labels.slice(3).join(SEP);
  }
  if (rest.startsWith('day-private'+SEP)) {params.privateMode=true;state.setPrivate(true);rest=rest.substring(('day-private'+SEP).length);}
  // `day-resource-site` is a provider's own site, policed by its base below; `day-resource` is a
  // provider attached to an ordinary view, which browses anywhere.
  let resourceSite: boolean = false;
  if (rest.startsWith('day-resource-site'+SEP)) {params.resources=true;resourceSite=true;rest=rest.substring(('day-resource-site'+SEP).length);}
  else if (rest.startsWith('day-resource'+SEP)) {params.resources=true;rest=rest.substring(('day-resource'+SEP).length);}
  if (rest.startsWith('day-transparent' + SEP)) {
    params.transparent = true;
    rest = rest.substring(('day-transparent' + SEP).length);
  }
  if (rest.startsWith('day-inline' + SEP)) {
    const parts: string[] = rest.split(SEP);
    const root: string = parts.length > 1 ? parts[1] : '';
    const start: string = parts.length > 2 && parts[2] !== '' ? parts[2] : 'index.html';
    params.inlinePrefix = 'resource://rawfile/day/' + root + '/';
    params.url = params.inlinePrefix + start;
  } else {
    params.url = rest;
    if(resourceSite)params.inlinePrefix=rest.substring(0,rest.indexOf('/','https://day-resource.invalid/'.length)+1);
  }
  if (desktop) {
    // Queued until the controller attaches, then run in order: the agent, then the first page
    // (a provider's view loads its first page itself, once its scheme is installed).
    state.command('desktop', '1');
    if (!params.resources && !params.filter) {
      params.deferLoad = true;
      state.command('load', params.url);
    }
  }
  const node: BuilderNode<[DayWebViewParams]> = new BuilderNode(ui);
  try {
    node.build(wrapBuilder<[DayWebViewParams]>(dayWebViewBuilder), params);
    const frame: FrameNode | null = node.getFrameNode();
    if (frame !== null) {
      instances.set(id, new DayWebViewInstance(node, state, params));
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
  if (live === undefined) {
    return;
  }
  // Settings (WebView::zoom, WebView::javascript): Web attributes, applied by updating the
  // builder with the changed parameters. A page cannot gain or lose its scripts while it runs.
  if (cmd === 'zoom' || cmd === 'javascript') {
    if (cmd === 'zoom') {
      live.params.textZoom = Number(arg);
    } else {
      live.params.scripts = arg === '1';
    }
    live.node.update(live.params);
    if (cmd === 'javascript') {
      live.controller.command('reload', '');
    }
    return;
  }
  // A filtered view's first page, once its scheme handler is installed: the Rust side asks
  // for it with an empty URL, since an inline site's address is composed here.
  live.controller.command(cmd, cmd === 'load' && arg === '' ? live.params.url : arg);
  // WebView::desktop_mode: a new agent, then the page again so the site answers it.
  if (cmd === 'desktop') {
    live.controller.command('reload', '');
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

// A dayscript screenshot waits for every live view's renderer frame (Controller.settle),
// all views at once.
function settle(): Promise<void> {
  const waits: Promise<void>[] = [];
  instances.forEach((live: DayWebViewInstance) => { waits.push(live.controller.settle()); });
  return Promise.all(waits).then((): void => {});
}

export const dayPiece: DayPieceModule = {
  kind: 'day.piece.webview',
  make: make,
  update: update,
  dispose: dispose,
  settle: settle
};
