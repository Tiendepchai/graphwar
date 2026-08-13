#!/usr/bin/env node
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import {spawn} from "node:child_process";
import tls from "node:tls";

const BASE = new URL(process.argv[2] ?? process.env.GRAPHWAR_URL ?? "http://127.0.0.1:8080");
const ORIGIN = BASE.origin;
const PASSWORD = `Graphwar-E2E-${crypto.randomBytes(18).toString("base64url")}!`;
const CHROME_CANDIDATES = process.env.CHROME_BIN
  ? [process.env.CHROME_BIN]
  : process.platform === "darwin"
    ? [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
      ]
    : process.platform === "win32"
      ? [
          `${process.env.PROGRAMFILES ?? "C:\\Program Files"}\\Google\\Chrome\\Application\\chrome.exe`,
          `${process.env["PROGRAMFILES(X86)"] ?? "C:\\Program Files (x86)"}\\Google\\Chrome\\Application\\chrome.exe`,
        ]
      : ["google-chrome", "chromium", "chromium-browser"];
const PROTOCOL_VERSION = 9;
const CAPTURE_PATH = process.env.E2E_CAPTURE_PATH ? path.resolve(process.env.E2E_CAPTURE_PATH) : null;
const timeoutMs = Number(process.env.E2E_TIMEOUT_MS ?? 15_000);
if (BASE.protocol === "https:" && process.env.E2E_TLS_VERIFY === "false") {
  process.env.NODE_TLS_REJECT_UNAUTHORIZED = "0";
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
function ok(condition, message) { assert.ok(condition, message); }
function log(message) { process.stdout.write(`[e2e] ${message}\n`); }
function fail(error) { process.stderr.write(`[e2e] FAIL: ${error.message}\n`); process.exitCode = 1; }
function withTimeout(promise, ms, label) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`timeout: ${label}`)), ms); }),
  ]).finally(() => clearTimeout(timer));
}

function cookieFrom(response) {
  const value = response.headers.get("set-cookie");
  ok(value, "login did not set a session cookie");
  const [pair] = value.split(";");
  return pair;
}

async function requireExpectedBuild() {
  const logout = await request("/auth/logout", {method: "POST"});
  ok(logout.status === 204, `wrong deployment: logout status ${logout.status}, expected 204`);
  ok(!logout.headers.has("set-cookie"), "wrong deployment: logout mutates cookies");
}

async function request(pathname, options = {}) {
  return fetch(new URL(pathname, BASE), {
    redirect: "manual",
    signal: AbortSignal.timeout(timeoutMs),
    ...options,
    headers: { ...(options.headers ?? {}) },
  });
}

async function jsonRequest(pathname, body, options = {}) {
  return request(pathname, {
    ...options,
    headers: { "content-type": "application/json", ...(options.headers ?? {}) },
    body: JSON.stringify(body),
  });
}

async function registerAndLogin(label) {
  const suffix = crypto.randomUUID().slice(0, 8);
  const user = {
    email: `e2e-${label}-${suffix}@example.test`,
    display_name: `E2E ${label}`,
    password: PASSWORD,
  };
  const registered = await jsonRequest("/auth/register", user, {method: "POST"});
  ok(registered.status === 201, `register status ${registered.status}`);
  const login = await jsonRequest("/auth/login", {
    email: user.email,
    password: user.password,
  }, {method: "POST"});
  ok(login.ok, `login status ${login.status}`);
  const cookie = cookieFrom(login);
  ok(/HttpOnly/i.test(login.headers.get("set-cookie")), "session cookie is not HttpOnly");
  ok(/SameSite=Lax/i.test(login.headers.get("set-cookie")), "session cookie lacks SameSite=Lax");
  if (BASE.protocol === "https:") {
    ok(/(?:^|;\s*)Secure(?:;|$)/i.test(login.headers.get("set-cookie")), "HTTPS session cookie is not Secure");
  }
  const account = await (await request("/auth/me", {headers: {cookie}})).json();
  ok(account.email === user.email, "authenticated identity mismatch");
  return {user, account, cookie};
}

function browserUser(label) {
  const suffix = crypto.randomUUID().slice(0, 8);
  return {
    email: `e2e-browser-${label}-${suffix}@example.test`,
    display_name: `Browser ${label}`,
    password: PASSWORD,
  };
}

async function httpChecks() {
  const revoked = await registerAndLogin("alpha");
  const session = await registerAndLogin("bravo");
  const logout = await request("/auth/logout", {method: "POST", headers: {cookie: revoked.cookie}});
  ok(logout.status === 204, `logout status ${logout.status}`);
  ok(!logout.headers.has("set-cookie"), "logout returned a stale-cookie mutation");
  const rejected = await request("/auth/me", {headers: {cookie: revoked.cookie}});
  ok(rejected.status === 401, `revoked session status ${rejected.status}`);
  log("HTTP auth, cookie flags, logout revocation: pass");
  return session;
}

function wsUrl() {
  const value = new URL(BASE);
  value.protocol = value.protocol === "https:" ? "wss:" : "ws:";
  value.pathname = "/ws";
  value.search = "";
  return value;
}

function socketConnect(url, headers) {
  const secure = url.protocol === "wss:";
  const host = url.hostname.replace(/^\[([^\]]+)\]$/, "$1");
  const options = {host, port: Number(url.port || (secure ? 443 : 80))};
  const socket = secure
    ? tls.connect({...options, servername: net.isIP(host) ? undefined : host, rejectUnauthorized: process.env.E2E_TLS_VERIFY !== "false"})
    : net.connect(options);
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = setTimeout(() => failOnce(new Error("socket connection timeout")), timeoutMs);
    const cleanup = () => {
      clearTimeout(timer);
      socket.off("error", failOnce);
      socket.off("timeout", onTimeout);
    };
    const failOnce = error => {
      if (settled) return;
      settled = true;
      cleanup();
      socket.destroy();
      reject(error);
    };
    const onTimeout = () => failOnce(new Error("socket connection timeout"));
    socket.once("error", failOnce);
    socket.once("timeout", onTimeout);
    socket.setTimeout(timeoutMs);
    socket.once(secure ? "secureConnect" : "connect", () => {
      const key = crypto.randomBytes(16).toString("base64");
      const lines = [
        `GET ${url.pathname} HTTP/1.1`,
        `Host: ${url.host}`,
        "Upgrade: websocket",
        "Connection: Upgrade",
        `Sec-WebSocket-Key: ${key}`,
        "Sec-WebSocket-Version: 13",
        ...Object.entries(headers).map(([name, value]) => `${name}: ${value}`),
        "",
        "",
      ];
      socket.write(lines.join("\r\n"));
      let buffer = Buffer.alloc(0);
      const onData = chunk => {
        buffer = Buffer.concat([buffer, chunk]);
        const boundary = buffer.indexOf("\r\n\r\n");
        if (boundary < 0) return;
        settled = true;
        cleanup();
        socket.off("data", onData);
        const header = buffer.subarray(0, boundary).toString("ascii");
        const status = Number(header.match(/^HTTP\/\d\.\d (\d+)/)?.[1] ?? 0);
        resolve({socket, status, buffer: buffer.subarray(boundary + 4)});
      };
      socket.on("data", onData);
    });
  });
}

class RawWs {
  constructor(handshake) {
    this.socket = handshake.socket;
    this.buffer = handshake.buffer;
    this.waiters = [];
    this.closed = false;
    this.socket.on("data", chunk => { this.buffer = Buffer.concat([this.buffer, chunk]); this.flush(); });
    this.socket.once("close", () => { this.closed = true; this.flush(); });
    this.socket.once("error", () => { this.closed = true; this.flush(); });
  }
  send(opcode, payload) {
    const mask = crypto.randomBytes(4);
    let header;
    if (payload.length < 126) header = Buffer.from([0x80 | opcode, 0x80 | payload.length]);
    else if (payload.length <= 0xffff) header = Buffer.from([0x80 | opcode, 0x80 | 126, payload.length >> 8, payload.length & 255]);
    else throw new Error("test frame too large");
    const masked = Buffer.from(payload);
    for (let index = 0; index < masked.length; index++) masked[index] ^= mask[index % 4];
    this.socket.write(Buffer.concat([header, mask, masked]));
  }
  sendText(text) { this.send(0x1, Buffer.from(text)); }
  sendPong(payload) { this.send(0xa, payload); }
  close() { this.socket.destroy(); }
  consume() {
    while (this.buffer.length >= 2) {
      const first = this.buffer[0];
      const second = this.buffer[1];
      const opcode = first & 0x0f;
      const masked = Boolean(second & 0x80);
      let length = second & 0x7f;
      let offset = 2;
      if (length === 126) {
        if (this.buffer.length < 4) return null;
        length = this.buffer.readUInt16BE(2); offset = 4;
      } else if (length === 127) {
        if (this.buffer.length < 10) return null;
        const value = this.buffer.readBigUInt64BE(2);
        if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error("unsupported frame length");
        length = Number(value); offset = 10;
      }
      const maskBytes = masked ? 4 : 0;
      if (this.buffer.length < offset + maskBytes + length) return null;
      const mask = masked ? this.buffer.subarray(offset, offset + 4) : null;
      offset += maskBytes;
      const payload = Buffer.from(this.buffer.subarray(offset, offset + length));
      this.buffer = this.buffer.subarray(offset + length);
      if (mask) for (let index = 0; index < payload.length; index++) payload[index] ^= mask[index % 4];
      if (opcode === 0x9) { this.sendPong(payload); continue; }
      return {
        opcode,
        closeCode: opcode === 0x8 && payload.length >= 2 ? payload.readUInt16BE(0) : undefined,
        text: opcode === 0x1 ? payload.toString() : "",
      };
    }
    return null;
  }
  flush() {
    while (this.waiters.length) {
      let frame;
      try { frame = this.consume(); } catch (error) { this.waiters.shift().reject(error); continue; }
      if (frame) { this.waiters.shift().resolve(frame); continue; }
      if (this.closed) this.waiters.shift().reject(new Error("websocket frame: socket closed"));
      else break;
    }
  }
  async next(label = "websocket frame") {
    return withTimeout(new Promise((resolve, reject) => {
      this.waiters.push({resolve, reject});
      this.flush();
    }), timeoutMs, label);
  }
}

async function rawWs(cookie, origin = ORIGIN) {
  const result = await socketConnect(wsUrl(), {Origin: origin, Cookie: cookie});
  ok(result.status === 101, `websocket handshake status ${result.status}`);
  return new RawWs(result);
}

async function openWs(cookie, origin = ORIGIN) {
  const socket = await rawWs(cookie, origin);
  socket.sendText(JSON.stringify({type: "hello", payload: {version: PROTOCOL_VERSION}}));
  const hello = JSON.parse((await socket.next("server Hello")).text);
  ok(hello.type === "hello" && hello.payload.version === PROTOCOL_VERSION, "server hello mismatch");
  const sync = JSON.parse((await socket.next("state sync")).text);
  ok(["state_sync", "left_room"].includes(sync.type), "state sync missing");
  const roomList = JSON.parse((await socket.next("room list")).text);
  ok(roomList.type === "room_list", "room list missing");
  return socket;
}

async function nextWsMessage(socket, type, label) {
  return nextWsMessageAny(socket, [type], label);
}

async function nextWsMessageAny(socket, types, label) {
  for (;;) {
    const message = JSON.parse((await socket.next(label)).text);
    if (types.includes(message.type)) return message;
  }
}

function browserWsMessage(cdp, type, label) {
  return browserWsMessageAny(cdp, [type], label);
}

function browserWsMessageAny(cdp, types, label) {
  return browserWsFrameAny(cdp, "Network.webSocketFrameReceived", types, label);
}

function browserWsSentMessageAny(cdp, types, label) {
  return browserWsFrameAny(cdp, "Network.webSocketFrameSent", types, label);
}

function browserWsFrameAny(cdp, event, types, label) {
  return withTimeout(new Promise(resolve => {
    cdp.on(event, ({response}) => {
      try {
        const message = JSON.parse(response.payloadData);
        if (types.includes(message.type)) resolve(message);
      } catch {}
    });
  }), timeoutMs, label);
}

async function wsBoundaryChecks(session) {
  const missing = await socketConnect(wsUrl(), {Origin: ORIGIN});
  ok(missing.status !== 101, "unauthenticated websocket upgraded");
  missing.socket.destroy();
  const wrongOrigin = await socketConnect(wsUrl(), {Origin: "https://not-graphwar.invalid", Cookie: session.cookie});
  ok(wrongOrigin.status === 403, `wrong-origin status ${wrongOrigin.status}`);
  wrongOrigin.socket.destroy();

  const unsupported = await rawWs(session.cookie);
  unsupported.sendText(JSON.stringify({type: "hello", payload: {version: 2}}));
  ok((await unsupported.next()).text.includes("unsupported protocol"), "unsupported version not rejected");
  unsupported.close();

  const firstCommand = await rawWs(session.cookie);
  firstCommand.sendText(JSON.stringify({type: "list_rooms"}));
  ok((await firstCommand.next()).text.includes("hello is required first"), "missing Hello not rejected");
  firstCommand.close();

  const invalid = await openWs(session.cookie);
  invalid.sendText("not-json");
  const invalidMessage = JSON.parse((await invalid.next()).text);
  ok(
    invalidMessage.type === "error" && invalidMessage.payload?.message.includes("invalid JSON message"),
    "invalid JSON not rejected",
  );
  invalid.close();

  const oversized = await openWs(session.cookie);
  oversized.sendText(JSON.stringify({type: "chat", payload: {text: "x".repeat(9_000)}}));
  const oversizedFrame = await withTimeout(new Promise(resolve => {
    oversized.socket.once("close", () => resolve(null));
    oversized.next("oversized websocket response")
      .then(resolve)
      .catch(() => resolve(null));
  }), timeoutMs, "oversized websocket termination");
  ok(
    oversizedFrame?.opcode === 0x8 && oversizedFrame.closeCode === 1009,
    `oversized frame close was ${oversizedFrame?.closeCode ?? "missing"}, expected 1009`,
  );
  oversized.close();

  const rate = await openWs(session.cookie);
  let rateLimited = false;
  for (let index = 0; index < 130 && !rateLimited; index++) {
    rate.sendText("not-json");
    const frame = await rate.next("rate limit response");
    if (frame.opcode !== 0x1) continue;
    const message = JSON.parse(frame.text);
    rateLimited = message.type === "error" && message.payload.code === "rate_limited";
  }
  ok(rateLimited, "websocket rate limit did not return rate_limited");
  rate.close();

  const logout = await request("/auth/logout", {method: "POST", headers: {cookie: session.cookie}});
  ok(logout.status === 204, "revocation setup failed");
  const revoked = await socketConnect(wsUrl(), {Origin: ORIGIN, Cookie: session.cookie});
  ok(revoked.status === 401, `revoked websocket status ${revoked.status}`);
  revoked.socket.destroy();
  log("WebSocket origin, auth, Hello, parsing, size, rate, revocation: pass");
}

class Cdp {
  constructor(url) { this.url = url; this.nextId = 0; this.pending = new Map(); this.events = new Map(); }
  async connect() {
    this.socket = new WebSocket(this.url);
    await withTimeout(new Promise((resolve, reject) => {
      this.socket.onopen = resolve; this.socket.onerror = reject;
    }), timeoutMs, "Chrome DevTools connection");
    this.socket.onmessage = event => {
      const message = JSON.parse(event.data);
      if (message.id) this.pending.get(message.id)?.(message);
      else this.events.get(message.method)?.forEach(handler => handler(message.params));
    };
  }
  command(method, params = {}) {
    const id = ++this.nextId;
    return withTimeout(new Promise((resolve, reject) => {
      this.pending.set(id, message => {
        this.pending.delete(id);
        if (message.error) reject(new Error(`${method}: ${message.error.message}`)); else resolve(message.result);
      });
      this.socket.send(JSON.stringify({id, method, params}));
    }), timeoutMs, `CDP ${method}`);
  }
  on(method, handler) { this.events.set(method, [...(this.events.get(method) ?? []), handler]); }
  async evaluate(expression, awaitPromise = true) {
    const result = await this.command("Runtime.evaluate", {expression, awaitPromise, returnByValue: true});
    if (result.exceptionDetails) {
      const details = result.exceptionDetails;
      const description = details.exception?.description ?? details.text ?? "exception";
      const line = Number.isInteger(details.lineNumber) ? details.lineNumber + 1 : "?";
      const column = Number.isInteger(details.columnNumber) ? details.columnNumber + 1 : "?";
      throw new Error(`browser evaluation failed at ${line}:${column}: ${description}`);
    }
    return result.result?.value;
  }
  close() { this.socket?.close(); }
}

async function chromeCommand() {
  for (const candidate of CHROME_CANDIDATES) {
    if (candidate.includes(path.sep)) {
      if (await fs.stat(candidate).then(() => true).catch(() => false)) return candidate;
    } else if (await new Promise(resolve => {
      const child = spawn("which", [candidate], {stdio: "ignore"});
      child.once("exit", code => resolve(code === 0));
      child.once("error", () => resolve(false));
    })) {
      return candidate;
    }
  }
  throw new Error(`Chrome not found; set CHROME_BIN. Checked: ${CHROME_CANDIDATES.join(", ")}`);
}

async function closeBrowser(browser) {
  if (!browser) return;
  browser.cdp.close();
  if (!browser.child.killed) browser.child.kill("SIGTERM");
  await withTimeout(new Promise(resolve => browser.child.once("exit", resolve)), timeoutMs, "Chrome shutdown")
    .catch(() => browser.child.kill("SIGKILL"));
  await fs.rm(browser.dir, {recursive: true, force: true, maxRetries: 5, retryDelay: 200});
}

async function launchBrowser(url) {
  ok(typeof WebSocket === "function", "Node 22+ required: global WebSocket unavailable");
  const chrome = await chromeCommand();
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), "graphwar-e2e-"));
  const port = 9300 + crypto.randomInt(500);
  const child = spawn(chrome, [
    "--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    ...(BASE.protocol === "https:" && process.env.E2E_TLS_VERIFY === "false" ? ["--ignore-certificate-errors"] : []),
    `--remote-debugging-port=${port}`, `--user-data-dir=${dir}`, "about:blank",
  ], {stdio: "ignore"});
  let page;
  for (let attempt = 0; attempt < 80; attempt++) {
    const targets = await fetch(`http://127.0.0.1:${port}/json/list`)
      .then(response => response.json())
      .catch(() => null);
    page = targets?.find(target => target.type === "page");
    if (page) break;
    await sleep(100);
  }
  ok(page?.webSocketDebuggerUrl, "Chrome page DevTools endpoint unavailable");
  const cdp = new Cdp(page.webSocketDebuggerUrl);
  await cdp.connect();
  await cdp.command("Page.enable");
  await cdp.command("Runtime.enable");
  await cdp.command("Network.enable");
  await cdp.command("Page.navigate", {url: url.toString()});
  return {cdp, child, dir};
}

async function browserWait(cdp, expression, label, ms = timeoutMs) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await cdp.evaluate(expression).catch(() => false)) return;
    await sleep(100);
  }
  throw new Error(`browser wait failed: ${label}`);
}

async function browserSessionCookie(cdp) {
  const cookie = (await cdp.command("Network.getAllCookies")).cookies
    .find(cookie => cookie.name === "graphwar_session");
  ok(cookie, "browser session cookie unavailable");
  return `${cookie.name}=${cookie.value}`;
}

async function browserExpireSession(browser) {
  const cookie = await browserSessionCookie(browser.cdp);
  const session = await request("/auth/me", {
    headers: {cookie},
  });
  ok(session.ok, "browser session authentication unavailable");
  const logout = await request("/auth/logout", {
    method: "POST",
    headers: {cookie},
  });
  ok(logout.status === 204, "external session revocation failed");
}

async function browserSet(cdp, selector, value) {
  const expression = `(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; const setter = Object.getOwnPropertyDescriptor(e.constructor.prototype, "value")?.set; setter?.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event("input", {bubbles:true})); e.dispatchEvent(new Event("change", {bubbles:true})); return true; })()`;
  ok(await cdp.evaluate(expression), `missing browser input ${selector}`);
}
async function browserSetPlayerSoldiers(cdp, name, value) {
  const expression = `(() => { const row = [...document.querySelectorAll('.player-slot')].find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(name)}); const e = row?.querySelector('.player-soldiers:not(:disabled)'); if (!e) return false; const setter = Object.getOwnPropertyDescriptor(e.constructor.prototype, "value")?.set; setter?.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event("input", {bubbles:true})); e.dispatchEvent(new Event("change", {bubbles:true})); return true; })()`;
  ok(await cdp.evaluate(expression), `missing soldier count control for ${name}`);
}
async function browserSubmit(cdp, selector) { ok(await cdp.evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; e.requestSubmit(); return true; })()`), `missing form ${selector}`); }
async function browserClick(cdp, selector) { ok(await cdp.evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; e.click(); return true; })()`), `missing control ${selector}`); }
async function browserClickWithDialog(cdp, selector, response) {
  let resolveDialog;
  const dialog = new Promise(resolve => { resolveDialog = resolve; });
  cdp.on("Page.javascriptDialogOpening", params => {
    if (resolveDialog) {
      const resolve = resolveDialog;
      resolveDialog = null;
      resolve(params);
    }
  });
  const click = browserClick(cdp, selector);
  const params = await withTimeout(dialog, timeoutMs, "browser password prompt");
  ok(params.type === "prompt" && params.message === "Room password", "unexpected password dialog");
  await cdp.command("Page.handleJavaScriptDialog", {
    accept: response !== null,
    ...(response === null ? {} : {promptText: response}),
  });
  await click;
}
async function browserText(cdp, selector) { return cdp.evaluate(`document.querySelector(${JSON.stringify(selector)})?.textContent ?? ""`); }
async function browserCaptureGameplay(cdp) {
  if (!CAPTURE_PATH) return;
  await fs.mkdir(path.dirname(CAPTURE_PATH), {recursive: true});
  const {data} = await cdp.command("Page.captureScreenshot", {
    format: "png",
    fromSurface: true,
    captureBeyondViewport: false,
  });
  ok(data, "gameplay screenshot is empty");
  await fs.writeFile(CAPTURE_PATH, Buffer.from(data, "base64"));
  log(`gameplay screenshot: ${CAPTURE_PATH}`);
}
async function browserInstallRenderProbe(cdp, key, selectors) {
  ok(await cdp.evaluate(`(() => {
    const key = ${JSON.stringify(key)};
    const selectors = ${JSON.stringify(selectors)};
    const app = document.querySelector('#app');
    const nodes = selectors.map(selector => document.querySelector(selector));
    if (!app || nodes.some(node => !node)) return false;
    let replacements = 0;
    const observer = new MutationObserver(() => { replacements += 1; });
    observer.observe(app, {childList: true});
    window.__graphwarRenderProbes ??= {};
    window.__graphwarRenderProbes[key] = {app, nodes, selectors, observer, get replacements() { return replacements; }};
    return true;
  })()`), `${key} render probe setup failed`);
}
async function browserAssertRenderStable(cdp, key, label) {
  ok(await cdp.evaluate(`(() => {
    const probe = window.__graphwarRenderProbes?.[${JSON.stringify(key)}];
    if (!probe) return false;
    const same = probe.app === document.querySelector('#app')
      && probe.nodes.every((node, index) => node === document.querySelector(probe.selectors[index]));
    probe.observer.disconnect();
    return same && probe.replacements === 0;
  })()`), `${label}: stable DOM was replaced`);
}
async function browserInstallGameRenderProbe(cdp) {
  await browserInstallRenderProbe(cdp, "game", [
    ".app-frame", "#screen", ".game-shell", "#game-canvas", "#fire-form", "#chat-form",
    "#function-input", ".soldier-name-labels", ".game-chat ul", ".fire-button", "#turn-timer",
  ]);
}
async function browserAssertGameStable(cdp, label) {
  await browserAssertRenderStable(cdp, "game", label);
}

async function browserRegister(browser, user) {
  const {cdp} = browser;
  await browserWait(cdp, "Boolean(document.querySelector('#register-form'))", "register screen");
  await browserSet(cdp, "#register-name", user.display_name);
  await browserSet(cdp, "#register-email", user.email);
  await browserSet(cdp, "#register-password", user.password);
  await browserSubmit(cdp, "#register-form");
  await browserWait(cdp, "Boolean(document.querySelector('#create-room-open'))", "lobby screen");
  await browserWait(cdp, "Boolean(document.querySelector('.connection.is-online'))", "lobby connection");
}
async function browserOpenCreate(cdp) {
  await browserClick(cdp, "#create-room-open");
  await browserWait(cdp, "document.querySelector('#create-room-dialog')?.open === true && document.activeElement?.id === 'room-name'", "create room dialog");
}
async function browserCreate(cdp, name, visibility, password = "", kind = "standard") {
  await browserOpenCreate(cdp);
  await browserSet(cdp, "#room-name", name);
  await browserSet(cdp, "#room-kind", kind);
  await cdp.evaluate(`document.querySelector('#room-visibility').value = ${JSON.stringify(visibility)}; document.querySelector('#room-visibility').dispatchEvent(new Event('change',{bubbles:true}))`);
  if (visibility === "private") await browserSet(cdp, "#room-password", password);
  await browserSubmit(cdp, "#create-room-form");
  await browserWait(cdp, "Boolean(document.querySelector('#room-title'))", "room screen");
}
async function browserLeave(cdp) {
  await browserClick(cdp, "#leave-room");
  await browserWait(cdp, "Boolean(document.querySelector('#create-room-open'))", "lobby after leave");
}

async function browserFlows() {
  const alpha = browserUser("alpha");
  const bravo = browserUser("bravo");
  const a = await launchBrowser(BASE);
  const b = await launchBrowser(BASE);
  try {
    await browserInstallRenderProbe(a.cdp, "login-notice", [
      ".app-frame", "#screen", ".login-shell", "#login-form", "#register-form", "#login-password",
    ]);
    await browserSet(a.cdp, "#login-email", "missing@example.test");
    await browserSet(a.cdp, "#login-password", "invalid-password-123");
    ok(await a.cdp.evaluate(`(() => {
      const input = document.querySelector('#login-password');
      input?.focus();
      input?.setSelectionRange(2, 8, 'forward');
      return Boolean(input);
    })()`), "missing login input");
    await browserSubmit(a.cdp, "#login-form");
    await browserWait(a.cdp, "document.querySelector('.notices')?.textContent.trim().length > 0", "login error notice");
    await browserAssertRenderStable(a.cdp, "login-notice", "login error notice");
    ok(await a.cdp.evaluate(`(() => {
      const input = document.querySelector('#login-password');
      return input?.value === 'invalid-password-123'
        && document.activeElement === input
        && input.selectionStart === 2
        && input.selectionEnd === 8
        && input.selectionDirection === 'forward';
    })()`), "login error replaced form or lost focus");
    await browserRegister(a, alpha);
    await browserRegister(b, bravo);
    await browserOpenCreate(b.cdp);
    await browserSet(b.cdp, "#room-name", "preserved-lobby-draft");
    ok(await b.cdp.evaluate(`(() => {
      const input = document.querySelector('#room-name');
      input?.focus();
      input?.setSelectionRange(2, 9, 'forward');
      return Boolean(input);
    })()`), "missing lobby draft input");
    await browserInstallRenderProbe(b.cdp, "lobby-list", [
      ".app-frame", "#screen", ".lobby-shell", "#create-room-dialog", "#create-room-form", "#room-name",
    ]);
    const publicName = `Public E2E ${crypto.randomUUID().slice(0, 8)}`;
    await browserCreate(a.cdp, publicName, "public");
    await browserWait(a.cdp, `(() => {
      const title = document.querySelector('#room-title');
      const teamTwo = document.querySelector('.team-roster-2');
      const row = document.querySelector('.player-slot');
      return parseFloat(getComputedStyle(title).fontSize) <= 52
        && document.querySelectorAll('.player-team').length === 0
        && document.querySelectorAll('.player-soldiers').length === 1
        && document.querySelectorAll('.remove-player').length === 0
        && teamTwo?.querySelectorAll('.player-slot').length === 0
        && teamTwo?.querySelector('.empty-team')?.tagName === 'P'
        && row?.querySelector('.player-soldiers')
        && !row?.querySelector('.remove-player');
    })()`, "compact owner roster");
    const publicNameJs = JSON.stringify(publicName);
    await browserWait(b.cdp, `Boolean([...document.querySelectorAll('.room-list li')].find(li => li.querySelector('strong')?.textContent === ${publicNameJs}))`, "public room listing");
    await browserAssertRenderStable(b.cdp, "lobby-list", "lobby room-list update");
    ok(await b.cdp.evaluate(`(() => {
      const input = document.querySelector('#room-name');
      return input?.value === 'preserved-lobby-draft'
        && document.activeElement === input
        && input.selectionStart === 2
        && input.selectionEnd === 9
        && input.selectionDirection === 'forward';
    })()`), "lobby draft focus and caret lost");
    ok(await b.cdp.evaluate(`(() => { const button = [...document.querySelectorAll('.room-list li')].find(li => li.querySelector('strong')?.textContent === ${publicNameJs})?.querySelector('.join-room'); if (!button) return false; return button.dataset.roomProtected === 'false' && button.getAttribute('aria-label') === ${JSON.stringify(`Join room ${publicName}`)} && (button.click(), true); })()`), "public room join control missing or protected");
    await browserWait(b.cdp, "Boolean(document.querySelector('#room-title'))", "public roster guest");
    await browserWait(a.cdp, `(() => {
      const teams = [...document.querySelectorAll('.team-roster')];
      const roster = Object.fromEntries(teams.map(team => [
        team.querySelector('ul')?.dataset.team,
        [...team.querySelectorAll('.player-slot strong')].map(player => player.textContent),
      ]));
      const guestRow = [...document.querySelectorAll('.player-slot')]
        .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
      const remove = guestRow?.querySelector('.remove-player');
      const select = guestRow?.querySelector('.player-soldiers');
      const removeRect = remove?.getBoundingClientRect();
      const selectRect = select?.getBoundingClientRect();
      return teams.length === 2
        && teams.map(team => team.querySelector('h3')?.textContent).join('|') === 'Team One|Team Two'
        && document.querySelectorAll('.roster li').length === 2
        && document.querySelectorAll('.player-team').length === 0
        && document.querySelectorAll('.player-soldiers').length === 2
        && document.querySelectorAll('.remove-player').length === 1
        && roster['1']?.includes(${JSON.stringify(alpha.display_name)})
        && roster['2']?.includes(${JSON.stringify(bravo.display_name)})
        && remove?.previousElementSibling?.querySelector('.player-soldiers') === select
        && removeRect?.width >= 44
        && removeRect?.height >= 44
        && selectRect?.right <= removeRect?.left;
    })()`, "public team roster synchronization");
    await browserWait(b.cdp, `document.querySelectorAll('.player-soldiers').length === 2 && document.querySelectorAll('.remove-player').length === 0`, "guest roster controls");
    for (const [cdp, owner] of [[a.cdp, true], [b.cdp, false]]) {
      ok(await cdp.evaluate(`(() => {
        const alpha = ${JSON.stringify(alpha.display_name)};
        const bravo = ${JSON.stringify(bravo.display_name)};
        const rowFor = name => [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === name);
        const alphaRow = rowFor(alpha);
        const bravoRow = rowFor(bravo);
        const targets = [...document.querySelectorAll('.team-drop-target')];
        return Boolean(alphaRow?.querySelector('.select-player')) === ${owner}
          && Boolean(bravoRow?.querySelector('.select-player'))
          && Boolean(alphaRow?.matches('[draggable="true"]')) === ${owner}
          && Boolean(bravoRow?.matches('[draggable="true"]'))
          && targets.length === 2
          && targets.every(target => target.tabIndex >= 0
            && target.getBoundingClientRect().width > 0
            && target.getBoundingClientRect().height >= 44
            && target.getAttribute('aria-disabled') === 'true');
      })()`), `${owner ? "owner" : "guest"} team-transfer permissions or targets missing`);
    }
    const guestSoldiers = await a.cdp.evaluate(`(() => {
      const row = [...document.querySelectorAll('.player-slot')]
        .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
      return row?.querySelector('.player-soldiers')?.value;
    })()`);
    ok(guestSoldiers, "guest soldier count missing before team move");
    await browserClick(a.cdp, "#ready-button");
    await browserClick(b.cdp, "#ready-button");
    await browserWait(a.cdp, "document.querySelector('#start-game')?.disabled === false", "ready before team move");
    ok(await a.cdp.evaluate(`(() => {
      const row = [...document.querySelectorAll('.player-slot')]
        .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
      const move = row?.querySelector('.select-player');
      move?.click();
      return move
        && row.closest('.team-roster')?.dataset.team === '2'
        && move.getAttribute('aria-pressed') === 'true'
        && document.querySelector('.team-drop-target[data-team="1"]')?.getAttribute('aria-disabled') === 'false'
        && document.querySelector('.team-drop-target[data-team="2"]')?.getAttribute('aria-disabled') === 'true'
        && document.querySelector('#roster-move-status')?.textContent.includes('selected');
    })()`), "team selection should not optimistically move a card");
    await browserClick(a.cdp, '.team-drop-target[data-team="1"]');
    for (const cdp of [a.cdp, b.cdp]) {
      await browserWait(cdp, `(() => {
        const row = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
        return row?.closest('.team-roster')?.dataset.team === '1'
          && document.querySelector('#start-game')?.disabled === true
          && row.querySelector('.player-soldiers')?.value === ${JSON.stringify(guestSoldiers)};
      })()`, "authoritative owner team move");
    }
    ok(await a.cdp.evaluate(`(() => {
      const button = [...document.querySelectorAll('.select-player')]
        .find(button => button.closest('.player-slot')?.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
      button?.focus();
      return document.activeElement === button;
    })()`), "owner move focus setup");
    ok(await b.cdp.evaluate(`(() => {
      const row = [...document.querySelectorAll('.player-slot')]
        .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
      const move = row?.querySelector('.select-player');
      move?.click();
      return Boolean(move) && document.querySelector('.team-drop-target[data-team="2"]')?.getAttribute('aria-disabled') === 'false';
    })()`), "guest self team selection missing");
    await browserClick(b.cdp, '.team-drop-target[data-team="2"]');
    for (const cdp of [a.cdp, b.cdp]) {
      await browserWait(cdp, `(() => {
        const row = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
        return row?.closest('.team-roster')?.dataset.team === '2'
          && row.querySelector('.player-soldiers')?.value === ${JSON.stringify(guestSoldiers)};
      })()`, "authoritative guest self team move");
    }
    await browserWait(a.cdp, `(() => {
      const active = document.activeElement;
      return active?.matches('.select-player')
        && active.closest('.player-slot')?.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)};
    })()`, "team refresh focus restoration");
    await browserWait(a.cdp, `document.querySelector('#announcements')?.textContent.includes(${JSON.stringify(`${bravo.display_name} moved to Team Two`)})`, "team move live announcement");
    ok(await a.cdp.evaluate(`(() => {
      const row = [...document.querySelectorAll('.player-slot')]
        .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(alpha.display_name)});
      const source = row?.closest('.team-roster')?.dataset.team;
      const target = source === '1' ? '2' : '1';
      const roster = document.querySelector('.team-roster-' + target);
      const data = new DataTransfer();
      row?.dispatchEvent(new DragEvent('dragstart', {bubbles: true, cancelable: true, dataTransfer: data}));
      const unchanged = row?.closest('.team-roster')?.dataset.team === source
        && data.getData('text/plain') === row?.dataset.playerId;
      roster?.dispatchEvent(new DragEvent('dragover', {bubbles: true, cancelable: true, dataTransfer: data}));
      roster?.dispatchEvent(new DragEvent('drop', {bubbles: true, cancelable: true, dataTransfer: data}));
      return unchanged;
    })()`), "desktop drag setup should retain the card until server confirmation");
    for (const cdp of [a.cdp, b.cdp]) {
      await browserWait(cdp, `(() => {
        const row = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(alpha.display_name)});
        return row?.closest('.team-roster')?.dataset.team === '2';
      })()`, "authoritative desktop drag move");
    }
    ok(await a.cdp.evaluate(`(() => {
      const row = [...document.querySelectorAll('.player-slot')]
        .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(alpha.display_name)});
      row?.querySelector('.select-player')?.click();
      return Boolean(row);
    })()`), "owner restore selection missing");
    await browserClick(a.cdp, '.team-drop-target[data-team="1"]');
    for (const cdp of [a.cdp, b.cdp]) {
      await browserWait(cdp, `(() => {
        const row = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(alpha.display_name)});
        return row?.closest('.team-roster')?.dataset.team === '1';
      })()`, "authoritative team restoration");
    }
    await browserWait(a.cdp, `(() => {
      const first = document.querySelector('.team-roster-1')?.getBoundingClientRect();
      const second = document.querySelector('.team-roster-2')?.getBoundingClientRect();
      return first && second && first.left < second.left && Math.abs(first.top - second.top) < 4
        && document.documentElement.scrollWidth <= innerWidth;
    })()`, "desktop team roster layout");
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 390, height: 844, deviceScaleFactor: 1, mobile: true});
    await browserWait(a.cdp, `(() => {
      const first = document.querySelector('.team-roster-1')?.getBoundingClientRect();
      const second = document.querySelector('.team-roster-2')?.getBoundingClientRect();
      return first && second && first.top < second.top && document.documentElement.scrollWidth <= innerWidth;
    })()`, "mobile team roster stack");
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
    const roomChatLayout = await a.cdp.evaluate(`(() => {
      const panel = document.querySelector('.room-chat');
      const list = panel?.querySelector('ul');
      const form = panel?.querySelector('#chat-form');
      const rect = panel?.getBoundingClientRect();
      const styles = panel ? getComputedStyle(panel) : null;
      return {
        className: panel?.className ?? null,
        position: styles?.position ?? null,
        bottomGap: rect ? innerHeight - rect.bottom : null,
        left: rect?.left ?? null,
        right: rect?.right ?? null,
        width: rect?.width ?? null,
        viewport: [innerWidth, innerHeight],
        transform: styles?.transform ?? null,
        backdropFilter: styles?.backdropFilter ?? styles?.webkitBackdropFilter ?? null,
        backgroundColor: styles?.backgroundColor ?? null,
        overflowY: list ? getComputedStyle(list).overflowY : null,
        hasForm: Boolean(form),
      };
    })()`);
    ok(roomChatLayout?.className?.includes('field-log')
      && !roomChatLayout?.className?.includes('game-chat')
      && roomChatLayout.position === 'fixed'
      && roomChatLayout.bottomGap >= 0
      && roomChatLayout.bottomGap <= 32
      && roomChatLayout.left >= 0
      && roomChatLayout.left <= 24
      && roomChatLayout.right <= roomChatLayout.viewport[0]
      && roomChatLayout.width <= 440
      && roomChatLayout.transform === 'none'
      && roomChatLayout.backdropFilter !== 'none'
      && /^rgba\([^,]+,[^,]+,[^,]+,\s*0\.[0-9]+\)$/.test(roomChatLayout.backgroundColor)
      && roomChatLayout.overflowY === 'auto'
      && roomChatLayout.hasForm,
    `room chat should be fixed at the viewport bottom: ${JSON.stringify(roomChatLayout)}`);
    const roomChatOverflow = await a.cdp.evaluate(`(() => {
      const panel = document.querySelector('.room-chat');
      const list = panel?.querySelector('ul');
      const form = panel?.querySelector('#chat-form');
      const shell = document.querySelector('.room-shell');
      const grid = document.querySelector('.room-grid');
      if (!panel || !list || !form || !shell || !grid) return null;
      const original = list.innerHTML;
      const rows = count => Array.from({length: count}, (_, index) =>
        '<li class="chat-message"><strong>Test:</strong> <span>Overflow message ' + index + '</span></li>'
      ).join('');
      const measure = () => ({
        panelHeight: panel.getBoundingClientRect().height,
        formTop: form.getBoundingClientRect().top,
        formBottom: form.getBoundingClientRect().bottom,
        shellHeight: shell.getBoundingClientRect().height,
        gridHeight: grid.getBoundingClientRect().height,
        documentHeight: document.documentElement.scrollHeight,
      });
      const empty = measure();
      list.innerHTML = rows(40);
      list.scrollTop = list.scrollHeight;
      const first = measure();
      list.innerHTML = rows(80);
      list.scrollTop = list.scrollHeight;
      const second = {
        ...measure(),
        scrollHeight: list.scrollHeight,
        clientHeight: list.clientHeight,
        scrollTop: list.scrollTop,
        overflowY: getComputedStyle(list).overflowY,
        maxHeight: getComputedStyle(list).maxHeight,
        panelPosition: getComputedStyle(panel).position,
        panelBottomGap: innerHeight - panel.getBoundingClientRect().bottom,
        formInside: form.getBoundingClientRect().top >= panel.getBoundingClientRect().top - 1
          && form.getBoundingClientRect().bottom <= panel.getBoundingClientRect().bottom + 1,
        horizontalOverflow: document.documentElement.scrollWidth > innerWidth,
      };
      list.innerHTML = original;
      return {empty, first, second};
    })()`);
    ok(roomChatOverflow
      && roomChatOverflow.second.scrollHeight > roomChatOverflow.second.clientHeight
      && roomChatOverflow.second.scrollTop > 0
      && roomChatOverflow.second.overflowY === 'auto'
      && roomChatOverflow.second.maxHeight === 'none'
      && roomChatOverflow.second.panelPosition === 'fixed'
      && roomChatOverflow.second.panelBottomGap >= 0
      && roomChatOverflow.second.panelBottomGap <= 32
      && roomChatOverflow.second.formInside
      && Math.abs(roomChatOverflow.first.panelHeight - roomChatOverflow.empty.panelHeight) < 1
      && Math.abs(roomChatOverflow.second.panelHeight - roomChatOverflow.first.panelHeight) < 1
      && Math.abs(roomChatOverflow.second.formTop - roomChatOverflow.first.formTop) < 1
      && Math.abs(roomChatOverflow.second.shellHeight - roomChatOverflow.first.shellHeight) < 1
      && Math.abs(roomChatOverflow.second.gridHeight - roomChatOverflow.first.gridHeight) < 1
      && Math.abs(roomChatOverflow.second.documentHeight - roomChatOverflow.first.documentHeight) < 1
      && !roomChatOverflow.second.horizontalOverflow,
    `room chat should scroll without growing the room layout: ${JSON.stringify(roomChatOverflow)}`);
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 390, height: 844, deviceScaleFactor: 1, mobile: true});
    const mobileRoomChat = await a.cdp.evaluate(`(() => {
      const panel = document.querySelector('.room-chat');
      const list = panel?.querySelector('ul');
      const form = panel?.querySelector('#chat-form');
      if (!panel || !list || !form) return null;
      const original = list.innerHTML;
      list.innerHTML = Array.from({length: 50}, (_, index) =>
        '<li class="chat-message"><strong>Test:</strong> <span>Mobile overflow message ' + index + '</span></li>'
      ).join('');
      list.scrollTop = list.scrollHeight;
      const panelRect = panel.getBoundingClientRect();
      const formRect = form.getBoundingClientRect();
      const result = {
        position: getComputedStyle(panel).position,
        overflowY: getComputedStyle(list).overflowY,
        scrollHeight: list.scrollHeight,
        clientHeight: list.clientHeight,
        scrollTop: list.scrollTop,
        bottomGap: innerHeight - panelRect.bottom,
        left: panelRect.left,
        right: panelRect.right,
        width: panelRect.width,
        viewportWidth: innerWidth,
        transform: getComputedStyle(panel).transform,
        formInside: formRect.top >= panelRect.top - 1 && formRect.bottom <= panelRect.bottom + 1,
        horizontalOverflow: document.documentElement.scrollWidth > innerWidth,
      };
      list.innerHTML = original;
      return result;
    })()`);
    ok(mobileRoomChat
      && mobileRoomChat.position === 'fixed'
      && mobileRoomChat.overflowY === 'auto'
      && mobileRoomChat.scrollHeight > mobileRoomChat.clientHeight
      && mobileRoomChat.scrollTop > 0
      && mobileRoomChat.bottomGap >= 0
      && mobileRoomChat.bottomGap <= 32
      && mobileRoomChat.left >= 0
      && mobileRoomChat.left <= 16
      && mobileRoomChat.right <= mobileRoomChat.viewportWidth
      && mobileRoomChat.width <= mobileRoomChat.viewportWidth
      && mobileRoomChat.transform === 'none'
      && mobileRoomChat.formInside
      && !mobileRoomChat.horizontalOverflow,
    `mobile room chat should remain bounded and usable: ${JSON.stringify(mobileRoomChat)}`);
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
    ok(await a.cdp.evaluate(`(() => {
      const input = document.querySelector('#chat-input');
      if (!input) return false;
      input.value = 'draft-chat';
      input.focus();
      input.setSelectionRange(2, 7, 'forward');
      return true;
    })()`), "missing draft chat input");
    await browserInstallRenderProbe(a.cdp, "room-roster", [
      ".app-frame", "#screen", ".room-shell", "#ready-button", "#chat-form", "#chat-input",
    ]);
    await browserSet(b.cdp, ".player-soldiers:not(:disabled)", "1");
    await browserWait(a.cdp, `(() => {
      const input = document.querySelector('#chat-input');
      return input?.value === 'draft-chat'
        && document.activeElement === input
        && input.selectionStart === 2
        && input.selectionEnd === 7
        && input.selectionDirection === 'forward';
    })()`, "draft focus and caret preservation");
    await browserAssertRenderStable(a.cdp, "room-roster", "room roster update");
    ok(await a.cdp.evaluate(`(() => { const input = document.querySelector('#chat-input'); if (!input) return false; input.value = ''; return true; })()`), "missing draft chat input");
    await browserSet(a.cdp, ".player-soldiers:not(:disabled)", "1");
    ok(await a.cdp.evaluate(`(() => {
      const input = document.querySelector('#chat-input');
      const form = document.querySelector('#chat-form');
      if (!input || !form) return false;
      input.value = 'public-chat';
      input.dispatchEvent(new Event('input', {bubbles: true}));
      form.requestSubmit();
      return true;
    })()`), "missing chat form");
    await browserWait(b.cdp, `(() => {
      const message = [...document.querySelectorAll('.room-chat .chat-message')]
        .find(row => row.textContent.includes('public-chat'));
      return message?.textContent.trim() === ${JSON.stringify(`${alpha.display_name}: public-chat`)};
    })()`, "chat delivery and name-message format");
    await browserClick(a.cdp, "#ready-button");
    await browserClick(b.cdp, "#ready-button");
    await browserWait(a.cdp, "document.querySelector('#start-game')?.disabled === false", "start enabled");
    await browserClick(a.cdp, "#start-game");
    await browserWait(a.cdp, "Boolean(document.querySelector('#game-canvas'))", "public game owner", 20_000);
    await browserWait(b.cdp, "Boolean(document.querySelector('#game-canvas'))", "public game guest", 20_000);
    await browserWait(a.cdp, "document.querySelectorAll('.soldier-name-label').length === 2 && document.querySelectorAll('.soldier-name-label.is-active').length === 1", "owner soldier labels", 20_000);
    await browserWait(b.cdp, "document.querySelectorAll('.soldier-name-label').length === 2 && document.querySelectorAll('.soldier-name-label.is-active').length === 1", "guest soldier labels", 20_000);
    const activeIsA = await a.cdp.evaluate("document.querySelector('#function-input')?.disabled === false");
    const activeName = activeIsA ? alpha.display_name : bravo.display_name;
    for (const cdp of [a.cdp, b.cdp]) {
      const battlefieldSemantics = await cdp.evaluate(`(() => {
        const canvas = document.querySelector('#game-canvas');
        const summary = document.querySelector('#battlefield-summary');
        const field = document.querySelector('.battlefield');
        const wrapper = document.querySelector('.soldier-name-labels');
        const labels = [...document.querySelectorAll('.soldier-name-label')];
        const activeLabel = document.querySelector('.soldier-name-label.is-active');
        const fieldRect = field?.getBoundingClientRect();
        const expectedNames = ${JSON.stringify([alpha.display_name, bravo.display_name])};
        const keys = labels.map(label => label.dataset.playerId + ':' + label.dataset.soldierIndex);
        const labelsFit = labels.every(label => {
          const labelRect = label.getBoundingClientRect();
          const x = Number.parseFloat(label.style.getPropertyValue('--soldier-x'));
          const y = Number.parseFloat(label.style.getPropertyValue('--soldier-y'));
          const soldierX = fieldRect?.left + fieldRect?.width * x / 100;
          const soldierY = fieldRect?.top + fieldRect?.height * y / 100;
          const horizontalGap = Math.max(0, labelRect.left - soldierX, soldierX - labelRect.right);
          const verticalGap = Math.max(0, labelRect.top - soldierY, soldierY - labelRect.bottom);
          return label.dataset.playerId
            && /^\\d+$/.test(label.dataset.soldierIndex)
            && Number.isFinite(soldierX) && Number.isFinite(soldierY)
            && labelRect.left >= fieldRect.left && labelRect.right <= fieldRect.right
            && labelRect.top >= fieldRect.top && labelRect.bottom <= fieldRect.bottom
            && horizontalGap <= 80
            && verticalGap <= 32;
        });
        const activeStyle = activeLabel && getComputedStyle(activeLabel);
        const passiveLabel = labels.find(label => !label.classList.contains('is-active'));
        const passiveStyle = passiveLabel && getComputedStyle(passiveLabel);
        return {
          canvas: canvas?.getAttribute('aria-label') === 'Graphwar battlefield'
            && canvas?.getAttribute('aria-describedby') === summary?.id,
          summary: summary?.textContent.includes('Team One')
            && summary?.textContent.includes('Team Two')
            && summary?.textContent.includes(${JSON.stringify(activeName)}),
          wrapper: wrapper?.getAttribute('aria-hidden') === 'true',
          labels: labels.length === 2
            && expectedNames.every(name => labels.some(label => label.textContent === name))
            && new Set(keys).size === labels.length,
          active: activeLabel?.textContent === ${JSON.stringify(activeName)}
            && activeStyle?.borderBottomColor !== passiveStyle?.borderBottomColor
            && activeStyle?.textShadow !== passiveStyle?.textShadow,
          labelsFit,
          layout: !document.querySelector('.scoreboard')
            && !document.body.textContent.includes('Field report')
            && document.querySelector('.map-panel')?.classList.contains('field-map')
            && document.querySelector('.game-chat')?.classList.contains('field-log')
            && getComputedStyle(document.querySelector('.game-chat')).position !== 'fixed'
            && !document.querySelector('.room-chat')
            && document.querySelectorAll('.notices').length === 0
            && Boolean(document.querySelector('.game-chat ul'))
            && !document.querySelector('.combat-log'),
        labelRects: labels.map(label => label.getBoundingClientRect().toJSON()),
        fieldRect: fieldRect?.toJSON(),
      };
      })()`);
      ok(Object.values(battlefieldSemantics).filter(value => typeof value === 'boolean').every(Boolean), `battlefield soldier labels or accessibility semantics missing: ${JSON.stringify(battlefieldSemantics)}`);
      ok(await cdp.evaluate(`(() => {
        const timer = document.querySelector('.fire-button #turn-timer');
        const heading = document.querySelector('.map-heading');
        const button = document.querySelector('.fire-button');
        const progress = Number.parseFloat(getComputedStyle(button).getPropertyValue('--turn-progress'));
        return heading?.querySelector('.eyebrow')?.textContent.trim() === 'Coordinate field / 01'
          && heading?.querySelector('h2')?.textContent.trim() === 'Battlefield'
          && timer?.getAttribute('role') === 'timer'
          && /^\\d+s$/.test(timer.textContent.trim())
          && button?.getAttribute('aria-describedby')?.split(/\\s+/).includes('turn-timer')
          && progress >= 0 && progress <= 100;
      })()`), "fire countdown semantics missing");
    }
    for (const viewport of [
      {width: 1920, height: 1080, deviceScaleFactor: 1},
      {width: 1280, height: 620, deviceScaleFactor: 1},
      {width: 1440, height: 900, deviceScaleFactor: 1},
    ]) {
      await a.cdp.command("Emulation.setDeviceMetricsOverride", {...viewport, mobile: false});
      await browserWait(a.cdp, `(() => {
        const map = document.querySelector('.map-panel')?.getBoundingClientRect();
        const stack = document.querySelector('.command-stack')?.getBoundingClientRect();
        const field = document.querySelector('.battlefield')?.getBoundingClientRect();
        const canvas = document.querySelector('#game-canvas')?.getBoundingClientRect();
        return map && stack && field && canvas
          && map.left < stack.left
          && map.width > stack.width
          && Math.abs(field.width / field.height - 770 / 450) < .03
          && Math.abs(canvas.width / canvas.height - 770 / 450) < .03
          && Math.abs(canvas.width - field.width) < 6
          && Math.abs(canvas.height - field.height) < 6
          && document.documentElement.scrollWidth <= ${viewport.width};
      })()`, `tactical layout ${viewport.width}x${viewport.height}`);
    }
    const active = activeIsA ? a.cdp : b.cdp;
    const inactive = activeIsA ? b.cdp : a.cdp;
    const initialProgress = Number.parseFloat(await active.evaluate(
      "getComputedStyle(document.querySelector('.fire-button')).getPropertyValue('--turn-progress')",
    ));
    await browserWait(active, `(() => {
      const value = Number.parseFloat(getComputedStyle(document.querySelector('.fire-button')).getPropertyValue('--turn-progress'));
      return Number.isFinite(value) && value < ${initialProgress};
    })()`, "fire countdown progress", 3_000);
    if (CAPTURE_PATH) {
      await active.command("Emulation.setDeviceMetricsOverride", {width: 1440, height: 900, deviceScaleFactor: 1, mobile: false});
      await browserSet(active, "#function-input", "sin(x)");
      await browserWait(active, "document.querySelector('#function-error')?.textContent === ''", "gameplay capture preview");
      await sleep(150);
      await browserCaptureGameplay(active);
    }
    const chatLayout = await a.cdp.evaluate(`(() => {
      const list = document.querySelector('.command-stack .game-chat ul');
      const panel = document.querySelector('.command-stack .game-chat');
      const form = document.querySelector('.command-stack .game-chat form');
      const stack = document.querySelector('.command-stack');
      const field = document.querySelector('.battlefield');
      if (!list || !panel || !form || !stack || !field) return null;
      const original = list.innerHTML;
      const stackHeight = stack.getBoundingClientRect().height;
      const panelHeight = panel.getBoundingClientRect().height;
      const formTop = form.getBoundingClientRect().top;
      const fieldTop = field.getBoundingClientRect().top;
      const documentHeight = document.documentElement.scrollHeight;
      list.innerHTML = Array.from({length: 80}, (_, index) => '<li><strong>Test</strong><span>Overflow message ' + index + '</span></li>').join('');
      list.scrollTop = list.scrollHeight;
      const result = {
        scrollHeight: list.scrollHeight,
        clientHeight: list.clientHeight,
        scrollTop: list.scrollTop,
        overflowY: getComputedStyle(list).overflowY,
        stackHeight: stack.getBoundingClientRect().height,
        panelHeight: panel.getBoundingClientRect().height,
        formTop: form.getBoundingClientRect().top,
        fieldTop: field.getBoundingClientRect().top,
        documentHeight: document.documentElement.scrollHeight,
      };
      list.innerHTML = original;
      return {...result, stackHeightBefore: stackHeight, panelHeightBefore: panelHeight, formTopBefore: formTop, fieldTopBefore: fieldTop, documentHeightBefore: documentHeight};
    })()`);
    ok(chatLayout
      && chatLayout.scrollHeight > chatLayout.clientHeight
      && chatLayout.scrollTop > 0
      && chatLayout.overflowY === 'auto'
      && Math.abs(chatLayout.stackHeight - chatLayout.stackHeightBefore) < 1
      && Math.abs(chatLayout.panelHeight - chatLayout.panelHeightBefore) < 1
      && Math.abs(chatLayout.formTop - chatLayout.formTopBefore) < 1
      && Math.abs(chatLayout.fieldTop - chatLayout.fieldTopBefore) < 1
      && Math.abs(chatLayout.documentHeight - chatLayout.documentHeightBefore) < 1,
    `game chat should scroll without growing the battlefield: ${JSON.stringify(chatLayout)}`);
    for (const cdp of [a.cdp, b.cdp]) await browserInstallGameRenderProbe(cdp);
    const previewExceptions = [];
    active.on("Runtime.exceptionThrown", ({exceptionDetails}) => previewExceptions.push(exceptionDetails.text ?? "browser exception"));
    await browserSet(active, "#function-input", String.raw`\unknown{x}`);
    await browserWait(active, "document.querySelector('#function-error')?.textContent.includes('byte 0')", "malformed LaTeX preview error");
    await browserSet(active, "#function-input", String.raw`\frac{\sin(x)}{\sqrt{2}}`);
    await browserWait(active, "document.querySelector('#function-error')?.textContent === ''", "nested LaTeX preview recovery");
    await browserSet(active, "#function-input", "sin(x)");
    await browserWait(active, "document.querySelector('#function-error')?.textContent === ''", "plain preview recovery");
    ok(await active.evaluate(`(() => {
      const input = document.querySelector('#function-input');
      input?.focus();
      input?.setSelectionRange(1, 4, 'backward');
      window.dispatchEvent(new Event('resize'));
      return document.querySelector('label[for="function-input"]')?.textContent === 'Function'
        && document.querySelector('#game-canvas')?.getAttribute('aria-label') === 'Graphwar battlefield'
        && !document.querySelector('.game-notices');
    })()`), "game semantics missing");
    await sleep(150);
    ok(await active.evaluate(`(() => {
      const input = document.querySelector('#function-input');
      return input?.value === 'sin(x)'
        && document.activeElement === input
        && input.selectionStart === 1
        && input.selectionEnd === 4
        && input.selectionDirection === 'backward';
    })()`), "function draft focus and caret lost");
    ok(previewExceptions.length === 0, `preview caused browser exception: ${previewExceptions.join(", ")}`);
    async function sendGameChat(cdp, text) {
      await browserSet(cdp, "#chat-input", text);
      await browserSubmit(cdp, "#chat-form");
    }
    const orderedFeed = `(() => {
      const rows = [...document.querySelectorAll('.game-chat ul > li')];
      const sequences = rows.map(row => Number(row.dataset.sequence));
      return rows.length >= 3
        && new Set(sequences).size === rows.length
        && sequences.every((sequence, index) => index === 0 || sequences[index - 1] < sequence)
        && rows.findIndex(row => row.textContent.includes('chat-before')) >= 0
        && rows.findIndex(row => row.textContent.includes('chat-before')) + 1 === rows.findIndex(row => row.classList.contains('shot-entry') && row.querySelector('code')?.textContent === 'sin(x)')
        && rows.findIndex(row => row.classList.contains('shot-entry') && row.querySelector('code')?.textContent === 'sin(x)') + 1 === rows.findIndex(row => row.textContent.includes('chat-after'));
    })()`;
    await sendGameChat(active, "chat-before");
    await browserWait(inactive, "document.querySelector('.game-chat ul')?.textContent.includes('chat-before')", "pre-shot chat delivery");
    await browserSubmit(active, "#fire-form");
    await browserWait(active, "document.querySelector('#turn-timer')?.textContent.includes('Resolving')", "authoritative shot", 20_000);
    await browserWait(inactive, "document.querySelector('#turn-timer')?.textContent.includes('Resolving')", "remote authoritative shot", 20_000);
    for (const cdp of [active, inactive]) {
      await browserWait(cdp, "document.querySelector('#shot-status')?.textContent.trim().length > 0", "visible shot outcome", 20_000);
      ok(await cdp.evaluate(`(() => {
        const status = document.querySelector('#shot-status');
        return status?.getAttribute('role') === 'status'
          && status?.getAttribute('aria-live') === 'polite';
      })()`), "shot outcome status semantics missing");
    }
    for (const cdp of [active, inactive]) {
      ok(await cdp.evaluate(`(() => {
        const button = document.querySelector('.fire-button');
        const labels = document.querySelectorAll('.soldier-name-label');
        const activeLabel = document.querySelector('.soldier-name-label.is-active');
        return button?.disabled
          && Number.parseFloat(getComputedStyle(button).getPropertyValue('--turn-progress')) === 0
          && labels.length === 2
          && activeLabel?.textContent === ${JSON.stringify(activeName)};
      })()`), "resolving shot should retain all names and highlight the shooter");
    }
    await sendGameChat(active, "chat-after");
    for (const cdp of [a.cdp, b.cdp]) {
      await browserWait(cdp, orderedFeed, "authoritative chat-shot-chat order");
      await browserAssertGameStable(cdp, "shot update");
    }
    for (const cdp of [a.cdp, b.cdp]) {
      await browserInstallRenderProbe(cdp, "game-chat", [
        ".app-frame", "#screen", ".game-shell", "#game-canvas", "#fire-form", "#chat-form", "#chat-input",
      ]);
    }
    await browserWait(inactive, `(() => {
      const input = document.querySelector('#function-input');
      const labels = document.querySelectorAll('.soldier-name-label');
      const activeLabel = document.querySelector('.soldier-name-label.is-active');
      return input?.disabled === false
        && labels.length === 2
        && activeLabel?.textContent === ${JSON.stringify(activeIsA ? bravo.display_name : alpha.display_name)};
    })()`, "next authoritative turn highlight", 20_000);
    await sendGameChat(active, "game-chat");
    await browserWait(inactive, `(() => {
      const feed = document.querySelector('.game-chat ul');
      return feed?.textContent.includes('game-chat')
        && [...feed.querySelectorAll('.shot-entry code')].some(code => code.textContent === 'sin(x)');
    })()`, "in-game unified feed delivery");
    for (const cdp of [a.cdp, b.cdp]) await browserAssertRenderStable(cdp, "game-chat", "in-game chat update");
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 320, height: 800, deviceScaleFactor: 2, mobile: false});
    await browserWait(a.cdp, `(() => {
      const field = document.querySelector('.battlefield')?.getBoundingClientRect();
      const canvas = document.querySelector('#game-canvas');
      const rect = canvas?.getBoundingClientRect();
      return field && canvas && rect
        && Math.abs(field.width / field.height - 770 / 450) < .03
        && Math.abs(rect.width / rect.height - 770 / 450) < .03
        && canvas.width === Math.round(rect.width * 2)
        && canvas.height === Math.round(rect.height * 2)
        && document.documentElement.scrollWidth <= 320;
    })()`, "responsive DPR canvas");
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 1440, height: 900, deviceScaleFactor: 1, mobile: false});
    await browserWait(a.cdp, "document.querySelector('#game-canvas')?.getBoundingClientRect().width > 320", "desktop canvas restore");
    await a.cdp.command("Page.reload");
    await browserWait(a.cdp, "Boolean(document.querySelector('#game-canvas'))", "state sync after refresh", 20_000);
    await browserWait(a.cdp, orderedFeed, "ordered feed after refresh");
    await browserWait(a.cdp, `(() => {
      const rows = [...document.querySelectorAll('.game-chat ul > li')];
      const sequences = rows.map(row => row.dataset.sequence);
      return rows.length >= 5 && new Set(sequences).size === rows.length;
    })()`, "deduplicated feed after refresh");
    await browserExpireSession(a);
    await browserWait(a.cdp, "Boolean(document.querySelector('#login-form'))", "external session expiration", 75_000);
    await browserWait(
      a.cdp,
      "document.querySelector('.notices')?.textContent.includes('Session expired; sign in again')",
      "session expiration notice",
      75_000,
    );
    await browserClick(b.cdp, "#logout");
    await browserWait(b.cdp, "Boolean(document.querySelector('#login-form'))", "logout screen");
    await browserWait(
      b.cdp,
      "fetch('/auth/me', {credentials: 'same-origin'}).then(response => response.status === 401)",
      "logout revocation",
    );
    await b.cdp.command("Page.reload");
    await browserWait(b.cdp, "Boolean(document.querySelector('#login-form'))", "logout survives reload");
    log("two-browser public room, chat, setup, readiness, start, fire, refresh, logout: pass");

    let e;
    let f;
    try {
      e = await launchBrowser(BASE);
      f = await launchBrowser(BASE);
      const practiceOwner = browserUser("practice-owner");
      const practiceGuest = browserUser("practice-guest");
      await browserRegister(e, practiceOwner);
      await browserRegister(f, practiceGuest);
      await browserCreate(e.cdp, "Standard E2E", "public");
      ok(!await e.cdp.evaluate("Boolean(document.querySelector('.practice-editor'))"), "standard room exposed practice editor");
      await browserLeave(e.cdp);

      const practiceName = `Practice E2E ${crypto.randomUUID().slice(0, 8)}`;
      await browserCreate(e.cdp, practiceName, "public", "", "practice");
      await browserWait(e.cdp, "Boolean(document.querySelector('.practice-editor') && document.querySelector('.practice-toolbar') && document.querySelector('#practice-soldier-form'))", "practice owner editor");
      await browserWait(f.cdp, `Boolean([...document.querySelectorAll('.room-list li')].find(li => li.querySelector('strong')?.textContent === ${JSON.stringify(practiceName)}))`, "practice room listing");
      const ownerPracticeJoin = browserWsMessage(e.cdp, "room", "practice owner join update");
      const roomId = await f.cdp.evaluate(`(() => {
        const row = [...document.querySelectorAll('.room-list li')]
          .find(li => li.querySelector('strong')?.textContent === ${JSON.stringify(practiceName)});
        const button = row?.querySelector('.join-room');
        if (!button) return null;
        const id = button.dataset.roomId;
        button.click();
        return id;
      })()`);
      ok(roomId, "practice join control missing");
      await ownerPracticeJoin;
      await browserWait(f.cdp, "Boolean(document.querySelector('.practice-editor'))", "practice guest editor");
      ok(await f.cdp.evaluate("Boolean(document.querySelector('.practice-readonly')) && !document.querySelector('.practice-toolbar') && !document.querySelector('#practice-soldier-form')"), "practice guest editor is not read-only");

      const practiceProtocol = await registerAndLogin("practice-protocol");
      const practiceWs = await openWs(practiceProtocol.cookie);
      try {
        practiceWs.sendText(JSON.stringify({type: "join_room", payload: {room_id: roomId, invite: null}}));
        const practiceJoined = await nextWsMessage(practiceWs, "room", "raw practice join");
        ok(practiceJoined.payload.snapshot.kind === "practice", "raw practice guest joined wrong room kind");
        practiceWs.sendText(JSON.stringify({
          type: "set_practice_setup",
          payload: {
            base_revision: practiceJoined.payload.snapshot.revision,
            setup: practiceJoined.payload.practice_setup,
          },
        }));
        const rejected = await nextWsMessage(practiceWs, "error", "guest practice setup rejection");
        ok(rejected.payload.code === "not_owner", "guest practice setup mutation was not rejected as not_owner");
        practiceWs.sendText(JSON.stringify({type: "leave_room"}));
        await nextWsMessage(practiceWs, "left_room", "raw practice guest leave");
      } finally {
        practiceWs.close();
      }

      await browserSetPlayerSoldiers(e.cdp, practiceOwner.display_name, "1");
      await browserWait(e.cdp, `(() => {
        const row = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(practiceOwner.display_name)});
        return row?.querySelector('.player-soldiers')?.value === '1'
          && document.querySelectorAll('#practice-soldier option').length === 3;
      })()`, "practice owner one-soldier setup");
      await browserSetPlayerSoldiers(f.cdp, practiceGuest.display_name, "1");
      await browserWait(e.cdp, "document.querySelectorAll('#practice-soldier option').length === 2 && [...document.querySelectorAll('.player-soldiers')].every(select => select.value === '1')", "practice one-soldier setup");
      await browserClick(e.cdp, "#ready-button");
      await browserClick(f.cdp, "#ready-button");
      await browserWait(e.cdp, "document.querySelector('#start-game')?.disabled === false", "practice ready before setup mutation");
      const byPlayerName = name => `(() => [...document.querySelectorAll('#practice-soldier option')].find(option => option.textContent.startsWith(${JSON.stringify(name)} + ' ·'))?.value ?? null)()`;
      const ownerSoldier = await e.cdp.evaluate(byPlayerName(practiceOwner.display_name));
      const guestSoldier = await e.cdp.evaluate(byPlayerName(practiceGuest.display_name));
      ok(ownerSoldier && guestSoldier, "practice soldier options missing");
      await browserSet(e.cdp, "#practice-soldier", ownerSoldier);
      await browserSet(e.cdp, "#practice-x", "100");
      await browserSet(e.cdp, "#practice-y", "225");
      await browserSubmit(e.cdp, "#practice-soldier-form");
      await browserWait(e.cdp, "document.querySelector('#practice-status')?.textContent.startsWith('Blank terrain is valid')", "practice owner placement sync");
      await browserSet(e.cdp, "#practice-soldier", guestSoldier);
      await browserSet(e.cdp, "#practice-x", "130");
      await browserSet(e.cdp, "#practice-y", "225");
      await browserSubmit(e.cdp, "#practice-soldier-form");
      await browserWait(e.cdp, "document.querySelector('#practice-status')?.textContent.startsWith('Blank terrain is valid')", "practice guest placement sync");
      const terrainPoints = [{x: 72, y: 225}, {x: 158, y: 225}];
      let mutatedSetup;
      for (const point of terrainPoints) {
        const practiceRoom = browserWsMessage(e.cdp, "room", "practice setup update");
        await browserSet(e.cdp, "#practice-terrain-x", String(point.x));
        await browserSet(e.cdp, "#practice-terrain-y", String(point.y));
        await browserSet(e.cdp, "#practice-terrain-radius", "20");
        await browserSubmit(e.cdp, "#practice-terrain-form");
        mutatedSetup = (await practiceRoom).payload.practice_setup;
      }
      ok(mutatedSetup?.terrain?.length === terrainPoints.length
        && terrainPoints.every((point, index) => mutatedSetup.terrain[index].x === point.x
          && mutatedSetup.terrain[index].y === point.y
          && mutatedSetup.terrain[index].radius === 20),
      `practice setup mutation mismatch: ${JSON.stringify(mutatedSetup)}`);
      await browserWait(e.cdp, "document.querySelectorAll('.practice-circle-list li').length === 2 && document.querySelector('#start-game')?.disabled === true", "practice mutation resets readiness");
      await browserWait(f.cdp, "document.querySelector('#practice-status')?.textContent === 'Setup synchronized.'", "practice setup guest synchronization");
      await f.cdp.command("Page.reload");
      await browserWait(f.cdp, "Boolean(document.querySelector('.practice-readonly'))", "practice guest reconnect");
      await browserWait(f.cdp, "document.querySelector('#practice-status')?.textContent === 'Setup synchronized.'", "practice setup reconnect persistence");
      await browserClick(e.cdp, "#ready-button");
      await browserClick(f.cdp, "#ready-button");
      await browserWait(e.cdp, "document.querySelector('#start-game')?.disabled === false", "practice start enabled");
      const practiceStart = browserWsMessage(e.cdp, "game_started", "practice game start");
      await browserClick(e.cdp, "#start-game");
      const started = (await practiceStart).payload.game;
      ok(JSON.stringify(started.terrain) === JSON.stringify(mutatedSetup.terrain)
        && mutatedSetup.players.every(placement => placement.soldiers.every((point, index) => {
          const soldier = started.soldiers.find(candidate => candidate.player_id === placement.player_id && candidate.index === index);
          return soldier?.x === point.x && soldier?.y === point.y;
        })),
      `practice game did not start from exact setup: ${JSON.stringify(started)}`);
      for (const cdp of [e.cdp, f.cdp]) {
        await browserWait(cdp, "Boolean(document.querySelector('#game-canvas'))", "practice game", 20_000);
      }
      log("practice owner/guest controls, rejection, setup readiness reset, reconnect, start: pass");

      // Finished -> ReturnToLobby -> rematch keeps the practice setup.
      const ownerShotWs = await openWs(await browserSessionCookie(e.cdp));
      const finishShot = new Promise((resolve, reject) => {
        ownerShotWs.sendText(JSON.stringify({type: "fire_function", payload: {function: "0", angle_deg: 0}}));
        (async () => {
          for (;;) {
            const message = JSON.parse((await ownerShotWs.next("practice decisive shot")).text);
            if (["game_finished", "shot_resolved", "error"].includes(message.type)) {
              resolve(message);
              return;
            }
          }
        })().catch(reject);
      }).finally(() => ownerShotWs.close());
      const finishMessage = await finishShot;
      ok(finishMessage.type === "game_finished", `practice decisive shot returned ${JSON.stringify(finishMessage)}`);
      ok(finishMessage.payload.snapshot.phase === "finished"
        && finishMessage.payload.shot.winner_team === 1
        && finishMessage.payload.shot.outcome.type === "terrain_impact"
        && finishMessage.payload.shot.outcome.payload.hits.length === 1,
      `practice decisive shot did not finish the game: ${JSON.stringify(finishMessage.payload.shot)}`);
      await browserWait(e.cdp, "document.querySelector('.finished-actions')?.hidden === false && document.querySelector('#return-to-lobby')?.disabled === false", "practice owner finished actions", 20_000);
      await browserWait(f.cdp, "document.querySelector('.finished-actions')?.hidden === false && document.querySelector('#return-to-lobby')?.disabled === true", "practice guest finished actions", 20_000);
      const ownerReturnWs = await openWs(await browserSessionCookie(e.cdp));
      const returned = new Promise((resolve, reject) => {
        ownerReturnWs.sendText(JSON.stringify({type: "return_to_lobby"}));
        nextWsMessage(ownerReturnWs, "room", "practice return-to-lobby response")
          .then(resolve)
          .catch(reject);
      }).finally(() => ownerReturnWs.close());
      const returnMessage = await returned;
      ok(returnMessage.payload.snapshot.phase === "lobby", `practice return-to-lobby returned ${JSON.stringify(returnMessage)}`);
      for (const cdp of [e.cdp, f.cdp]) {
        await browserWait(cdp, "Boolean(document.querySelector('#start-game'))", "practice return-to-lobby rematch screen", 20_000);
      }
      await browserWait(e.cdp, "document.querySelectorAll('.practice-circle-list li').length === 2", "practice setup survived return to lobby");
      await browserWait(f.cdp, "document.querySelector('#practice-status')?.textContent === 'Setup synchronized.'", "guest setup survived return to lobby");
      await browserClick(e.cdp, "#ready-button");
      await browserClick(f.cdp, "#ready-button");
      await browserWait(e.cdp, "document.querySelector('#start-game')?.disabled === false", "practice rematch start enabled");
      const rematchStart = browserWsMessage(e.cdp, "game_started", "practice rematch start");
      await browserClick(e.cdp, "#start-game");
      const rematch = (await rematchStart).payload.game;
      ok(JSON.stringify(rematch.terrain) === JSON.stringify(mutatedSetup.terrain),
        `practice rematch lost the setup terrain: ${JSON.stringify(rematch.terrain)}`);
      log("practice finished, return-to-lobby, rematch: pass");
    } finally {
      for (const browser of [e, f]) await closeBrowser(browser);
    }

    let c;
    let d;
    try {
      c = await launchBrowser(BASE);
      d = await launchBrowser(BASE);
      const privateGuest = browserUser("private-guest");
      await browserRegister(c, browserUser("private-owner"));
      await browserRegister(d, privateGuest);
      await d.cdp.command("Emulation.setDeviceMetricsOverride", {width: 320, height: 800, deviceScaleFactor: 1, mobile: false});
      ok(await d.cdp.evaluate(`(() => {
        const button = document.querySelector('#create-room-open');
        return getComputedStyle(document.documentElement).colorScheme === 'light'
          && button?.getBoundingClientRect().right <= 320
          && button?.getBoundingClientRect().height >= 44
          && !document.querySelector('#create-room-dialog')?.open
          && !document.querySelector('#invite-room-form')
          && document.querySelectorAll('.room-card').length >= 0
          && document.documentElement.scrollWidth <= 320;
      })()`), "mobile lobby create control overflow or misses touch target");
      await browserOpenCreate(d.cdp);
      const mobileCreateLayout = await d.cdp.evaluate(`(() => {
        const dialog = document.querySelector('#create-room-dialog');
        const form = document.querySelector('#create-room-form');
        const password = document.querySelector('#room-password');
        const controls = [...(form?.querySelectorAll('button, input, select') ?? [])]
          .filter(control => control.offsetParent);
        return {
          open: Boolean(dialog?.open),
          dialogLeft: dialog?.getBoundingClientRect().left,
          dialogRight: dialog?.getBoundingClientRect().right,
          formRight: form?.getBoundingClientRect().right,
          passwordHidden: !document.querySelector('#room-password-field')?.offsetParent,
          passwordDisabled: password?.disabled,
          controlHeights: controls.map(control => control.getBoundingClientRect().height),
          scrollWidth: document.documentElement.scrollWidth,
          viewportWidth: innerWidth,
        };
      })()`);
      ok(mobileCreateLayout.open
        && mobileCreateLayout.dialogLeft >= 0
        && mobileCreateLayout.dialogRight <= mobileCreateLayout.viewportWidth
        && mobileCreateLayout.formRight <= mobileCreateLayout.viewportWidth
        && mobileCreateLayout.passwordHidden
        && mobileCreateLayout.passwordDisabled
        && mobileCreateLayout.controlHeights.every(height => height >= 44)
        && mobileCreateLayout.scrollWidth <= mobileCreateLayout.viewportWidth,
      `mobile create dialog overflow or misses public form state: ${JSON.stringify(mobileCreateLayout)}`);
      await browserClick(d.cdp, "#create-room-cancel");
      await browserWait(d.cdp, "document.querySelector('#create-room-dialog')?.open === false", "create room cancel");
      await browserWait(d.cdp, "document.activeElement?.id === 'create-room-open'", "create room cancel opener focus");
      await browserOpenCreate(d.cdp);
      await d.cdp.evaluate(`document.querySelector('#room-visibility').value = 'private'; document.querySelector('#room-visibility').dispatchEvent(new Event('change',{bubbles:true}))`);
      ok(await d.cdp.evaluate(`(() => {
        const field = document.querySelector('#room-password-field');
        const password = document.querySelector('#room-password');
        return !field?.hidden && !password?.disabled && password?.required;
      })()`), "private room password field did not appear");
      await browserSet(d.cdp, "#room-password", "discarded-password");
      await d.cdp.evaluate(`document.querySelector('#room-visibility').value = 'public'; document.querySelector('#room-visibility').dispatchEvent(new Event('change',{bubbles:true}))`);
      ok(await d.cdp.evaluate(`document.querySelector('#room-password-field')?.hidden && document.querySelector('#room-password')?.disabled && document.querySelector('#room-password')?.value === ''`), "public room kept a password field or draft");
      await d.cdp.command("Input.dispatchKeyEvent", {type: "keyDown", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27});
      await d.cdp.command("Input.dispatchKeyEvent", {type: "keyUp", key: "Escape", code: "Escape", windowsVirtualKeyCode: 27, nativeVirtualKeyCode: 27});
      await browserWait(d.cdp, "document.querySelector('#create-room-dialog')?.open === false", "create room escape");
      await browserWait(d.cdp, "document.activeElement?.id === 'create-room-open'", "create room escape opener focus");
      await d.cdp.command("Emulation.setDeviceMetricsOverride", {width: 1280, height: 800, deviceScaleFactor: 1, mobile: false});
      const privateName = `Private E2E ${crypto.randomUUID().slice(0, 8)}`;
      const roomPassword = `room-${crypto.randomUUID()}`;
      await browserCreate(c.cdp, privateName, "private", roomPassword);
      const privateNameJs = JSON.stringify(privateName);
      await browserWait(d.cdp, `Boolean([...document.querySelectorAll('.room-list li')].find(li => li.querySelector('strong')?.textContent === ${privateNameJs}))`, "guest protected room listing");
      const privateRoom = await d.cdp.evaluate(`(() => {
        const card = [...document.querySelectorAll('.room-list li')]
          .find(li => li.querySelector('strong')?.textContent === ${privateNameJs});
        const button = card?.querySelector('.join-room');
        return card && button ? {
          id: button.dataset.roomId,
          protected: button.dataset.roomProtected,
          label: button.getAttribute('aria-label'),
          lockText: card.querySelector('.room-lock .sr-only')?.textContent,
          buttonHeight: button.getBoundingClientRect().height,
        } : null;
      })()`);
      ok(privateRoom?.id
        && privateRoom.protected === 'true'
        && privateRoom.label === `Join protected room ${privateName}`
        && privateRoom.lockText === 'Protected room'
        && privateRoom.buttonHeight >= 44,
      `protected room card semantics missing: ${JSON.stringify(privateRoom)}`);
      ok(!await d.cdp.evaluate(`document.body.textContent.includes(${JSON.stringify(roomPassword)})`), "room password leaked into browser DOM");
      const privateProtocol = await registerAndLogin("private-protocol");
      const privateWs = await openWs(privateProtocol.cookie);
      privateWs.sendText(JSON.stringify({type: "list_rooms"}));
      const privateRooms = JSON.parse((await privateWs.next("private websocket room list")).text);
      const listedPrivate = privateRooms.payload.rooms.find(room => room.id === privateRoom.id);
      ok(
        privateRooms.type === "room_list"
          && listedPrivate?.visibility === "private"
          && !JSON.stringify(privateRooms).includes(roomPassword),
        "private room missing from credential-free protocol listing",
      );
      for (const invite of [null, "wrong-password"]) {
        privateWs.sendText(JSON.stringify({type: "join_room", payload: {room_id: privateRoom.id, invite}}));
        const rejected = JSON.parse((await privateWs.next("private credential rejection")).text);
        ok(rejected.type === "error" && rejected.payload.code === "private", "invalid room password was accepted");
      }
      privateWs.sendText(JSON.stringify({type: "join_room", payload: {room_id: privateRoom.id, invite: roomPassword}}));
      const joined = JSON.parse((await privateWs.next("private raw websocket join")).text);
      ok(joined.type === "room" && joined.payload.snapshot.id === privateRoom.id, "correct raw password was rejected");
      const joinedList = JSON.parse((await privateWs.next("private join room list")).text);
      ok(joinedList.type === "room_list", "private join room list missing");
      privateWs.sendText(JSON.stringify({type: "leave_room"}));
      const left = JSON.parse((await privateWs.next("private raw websocket leave")).text);
      ok(left.type === "left_room", `raw protected guest could not leave: ${JSON.stringify(left)}`);
      privateWs.close();
      await browserInstallRenderProbe(d.cdp, "lobby-notice", [
        ".app-frame", "#screen", ".lobby-shell", "#create-room-open", "#create-room-dialog",
      ]);
      const privateButton = `.join-room[data-room-id="${privateRoom.id}"]`;
      await browserClickWithDialog(d.cdp, privateButton, null);
      await sleep(150);
      ok(await d.cdp.evaluate("Boolean(document.querySelector('#create-room-open')) && !document.querySelector('#create-room-dialog')?.open && !document.querySelector('.notices')?.textContent.includes('room is private')"), "cancelled password prompt changed lobby");
      await browserClickWithDialog(d.cdp, privateButton, "");
      await browserWait(d.cdp, "document.querySelector('.notices')?.textContent.includes('Room password is required')", "empty password notice");
      await browserAssertRenderStable(d.cdp, "lobby-notice", "empty password notice");
      await browserInstallRenderProbe(d.cdp, "lobby-wrong-password", [
        ".app-frame", "#screen", ".lobby-shell", "#create-room-open", "#create-room-dialog",
      ]);
      await browserClickWithDialog(d.cdp, privateButton, "wrong-password");
      await browserWait(d.cdp, "document.querySelector('.notices')?.textContent.includes('room is private')", "wrong password rejection");
      await browserWait(d.cdp, "document.querySelector('#announcements')?.textContent.includes('room is private')", "wrong password live error");
      await browserAssertRenderStable(d.cdp, "lobby-wrong-password", "wrong password notice");
      ok(!await d.cdp.evaluate(`document.body.textContent.includes(${JSON.stringify(roomPassword)})`), "room password leaked after failed join");
      await browserClickWithDialog(d.cdp, privateButton, roomPassword);
      await browserWait(d.cdp, "Boolean(document.querySelector('#room-title'))", "protected password join");
      await browserWait(c.cdp, `(() => {
        const row = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('strong')?.textContent === ${JSON.stringify(privateGuest.display_name)});
        return row?.querySelector('.remove-player')?.getAttribute('data-is-bot') === 'false';
      })()`, "protected guest kick control");
      await browserClick(c.cdp, ".remove-player");
      await browserWait(d.cdp, "Boolean(document.querySelector('#create-room-open'))", "kicked protected guest lobby");
      await browserWait(c.cdp, `(() => {
        const players = [...document.querySelectorAll('.player-slot strong')].map(row => row.textContent);
        return players.length === 1 && !players.includes(${JSON.stringify(privateGuest.display_name)});
      })()`, "owner roster after kick");
      log("protected room listing, password enforcement, prompt flow, and kick: pass");
      await browserLeave(c.cdp);

      await browserCreate(c.cdp, "Bot E2E", "public", "", "practice");
      await browserClick(c.cdp, "#add-bot");
      await browserWait(c.cdp, `(() => {
        const rows = document.querySelectorAll('.player-slot');
        const selectors = document.querySelectorAll('.player-soldiers');
        const remove = document.querySelector('.remove-player');
        return rows.length === 2
          && selectors.length === 2
          && remove?.getAttribute('data-is-bot') === 'true'
          && document.querySelectorAll('.player-team').length === 0;
      })()`, "bot slot");
      const botTarget = await c.cdp.evaluate(`(() => {
        const bot = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('.remove-player')?.dataset.isBot === 'true');
        const source = bot?.closest('.team-roster')?.dataset.team;
        const target = source === '1' ? '2' : '1';
        const move = bot?.querySelector('.select-player');
        move?.click();
        return move?.getAttribute('aria-pressed') === 'true'
          && document.querySelector('.team-drop-target[data-team="' + target + '"]')?.getAttribute('aria-disabled') === 'false'
          ? target
          : null;
      })()`);
      ok(botTarget, "owner bot transfer selection missing");
      await browserClick(c.cdp, `#team-${botTarget}-target`);
      await browserWait(c.cdp, `(() => {
        const bot = [...document.querySelectorAll('.player-slot')]
          .find(row => row.querySelector('.remove-player')?.dataset.isBot === 'true');
        return bot?.closest('.team-roster')?.dataset.team === ${JSON.stringify(botTarget)};
      })()`, "authoritative owner bot move");
      await browserClick(c.cdp, ".remove-player");
      await browserWait(c.cdp, "document.querySelectorAll('.player-slot').length === 1", "bot removal");
      await browserClick(c.cdp, "#add-bot");
      await browserWait(c.cdp, "document.querySelectorAll('.player-slot').length === 2 && document.querySelector('.remove-player')?.getAttribute('data-is-bot') === 'true'", "replacement bot slot");
      await browserClick(c.cdp, "#ready-button");
      await browserWait(c.cdp, "document.querySelector('#start-game')?.disabled === false", "bot start enabled");
      await c.cdp.command("Emulation.setEmulatedMedia", {media: "", features: [{name: "prefers-color-scheme", value: "dark"}]});
      await browserWait(c.cdp, "matchMedia('(prefers-color-scheme: dark)').matches", "dark system scheme");
      await browserClick(c.cdp, "#start-game");
      await browserWait(c.cdp, "Boolean(document.querySelector('#game-canvas'))", "bot game", 20_000);
      ok(await c.cdp.evaluate(`(() => {
        const canvas = document.querySelector('#game-canvas');
        const root = document.documentElement;
        const paper = getComputedStyle(root).getPropertyValue('--paper').trim();
        if (getComputedStyle(root).colorScheme !== 'light' || paper !== '#f4ecd8') return false;
        const pixels = canvas?.getContext('2d')?.getImageData(0, 0, canvas.width, canvas.height).data;
        if (!canvas?.width || !canvas?.height || !pixels) return false;
        for (let index = 0; index < pixels.length; index += 4) {
          if (pixels[index] === 244 && pixels[index + 1] === 236 && pixels[index + 2] === 216) return true;
        }
        return false;
      })()`), "battlefield should remain light under dark system scheme");
      await browserWait(c.cdp, "document.querySelector('#function-input')?.disabled === false", "human bot-game turn", 20_000);
      const botShotWs = await openWs(await browserSessionCookie(c.cdp));
      botShotWs.sendText(JSON.stringify({type: "fire_function", payload: {function: "0", angle_deg: 0}}));
      const botShot = await nextWsMessageAny(botShotWs, ["shot_resolved", "game_finished", "error"], "human shot before bot");
      botShotWs.close();
      ok(botShot.type === "shot_resolved"
        && botShot.payload.shot.outcome.type === "miss"
        && botShot.payload.shot.outcome.payload.reason === "world_exit",
      `human bot-game shot failed: ${JSON.stringify(botShot)}`);
      await browserWait(c.cdp, "document.querySelector('#shot-status')?.textContent.includes('Shot missed: trajectory left the battlefield')", "world-exit miss status", 20_000);
      ok(await c.cdp.evaluate(`(() => {
        const canvas = document.querySelector('#game-canvas');
        const status = document.querySelector('#shot-status');
        return Boolean(canvas) && status?.textContent.includes('Shot missed');
      })()`), "world-exit miss should render without an explosion status");
      await browserWait(
        c.cdp,
        "document.querySelector('#function-input')?.disabled === false",
        "bot completed authoritative turn",
        30_000,
      );
      await browserWait(
        c.cdp,
        "document.querySelectorAll('.game-chat .shot-entry').length >= 2",
        "bot function history",
      );
      await c.cdp.command("Emulation.setEmulatedMedia", {media: "", features: []});
      log("browser bot match and bot turn completion: pass");
    } finally {
      for (const browser of [c, d]) await closeBrowser(browser);
    }
  } finally {
    for (const browser of [a, b]) await closeBrowser(browser);
  }
}

async function main() {
  ok(["http:", "https:"].includes(BASE.protocol), "endpoint must use http:// or https://");
  const response = await request("/healthz");
  ok(response.ok, `healthz status ${response.status}`);
  await requireExpectedBuild();
  const session = await httpChecks();
  await wsBoundaryChecks(session);
  await browserFlows();
  log("all local delivery gates: pass");
}

main().catch(fail);
