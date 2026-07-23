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
const PROTOCOL_VERSION = 3;
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

async function openWs(cookie, origin = ORIGIN) {
  const result = await socketConnect(wsUrl(), {Origin: origin, Cookie: cookie});
  ok(result.status === 101, `websocket handshake status ${result.status}`);
  const socket = new RawWs(result);
  const hello = JSON.parse((await socket.next()).text);
  ok(hello.type === "hello" && hello.payload.version === PROTOCOL_VERSION, "server hello mismatch");
  return socket;
}

async function wsBoundaryChecks(session) {
  const missing = await socketConnect(wsUrl(), {Origin: ORIGIN});
  ok(missing.status !== 101, "unauthenticated websocket upgraded");
  missing.socket.destroy();
  const wrongOrigin = await socketConnect(wsUrl(), {Origin: "https://not-graphwar.invalid", Cookie: session.cookie});
  ok(wrongOrigin.status === 403, `wrong-origin status ${wrongOrigin.status}`);
  wrongOrigin.socket.destroy();

  const unsupported = await openWs(session.cookie);
  unsupported.sendText(JSON.stringify({type: "hello", payload: {version: 2}}));
  ok((await unsupported.next()).text.includes("unsupported protocol"), "unsupported version not rejected");
  unsupported.close();

  const firstCommand = await openWs(session.cookie);
  firstCommand.sendText(JSON.stringify({type: "list_rooms"}));
  ok((await firstCommand.next()).text.includes("hello is required first"), "missing Hello not rejected");
  firstCommand.close();

  const invalid = await openWs(session.cookie);
  invalid.sendText("not-json");
  ok((await invalid.next()).text.includes("invalid JSON message"), "invalid JSON not rejected");
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
  for (let index = 0; index < 121; index++) rate.sendText("not-json");
  let rateLimited = false;
  for (let index = 0; index < 121; index++) {
    const frame = await rate.next("rate limit response");
    if (frame.opcode !== 0x1) continue;
    const message = JSON.parse(frame.text);
    if (message.type === "error" && message.payload.code === "rate_limited") {
      rateLimited = true;
      break;
    }
  }
  ok(rateLimited, "websocket rate limit did not return rate_limited");
  rate.close();

  const revoked = await openWs(session.cookie);
  const logout = await request("/auth/logout", {method: "POST", headers: {cookie: session.cookie}});
  ok(logout.status === 204, "revocation setup failed");
  revoked.sendText(JSON.stringify({type: "hello", payload: {version: PROTOCOL_VERSION}}));
  const expired = JSON.parse((await revoked.next("session expiration frame")).text);
  ok(expired.type === "session_expired", "revoked websocket missed session_expired");
  await withTimeout(new Promise(resolve => revoked.socket.once("close", resolve)), timeoutMs, "revoked websocket close");
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
    if (result.exceptionDetails) throw new Error(`browser evaluation failed: ${result.exceptionDetails.text ?? "exception"}`);
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

async function browserExpireSession(browser) {
  const cookie = (await browser.cdp.command("Network.getAllCookies")).cookies
    .find(cookie => cookie.name === "graphwar_session");
  ok(cookie, "browser session cookie unavailable");
  const session = await request("/auth/me", {
    headers: {cookie: `${cookie.name}=${cookie.value}`},
  });
  ok(session.ok, "browser session authentication unavailable");
  const logout = await request("/auth/logout", {
    method: "POST",
    headers: {cookie: `${cookie.name}=${cookie.value}`},
  });
  ok(logout.status === 204, "external session revocation failed");
}

async function browserSet(cdp, selector, value) {
  const expression = `(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; const setter = Object.getOwnPropertyDescriptor(e.constructor.prototype, "value")?.set; setter?.call(e, ${JSON.stringify(value)}); e.dispatchEvent(new Event("input", {bubbles:true})); e.dispatchEvent(new Event("change", {bubbles:true})); return true; })()`;
  ok(await cdp.evaluate(expression), `missing browser input ${selector}`);
}
async function browserSubmit(cdp, selector) { ok(await cdp.evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; e.requestSubmit(); return true; })()`), `missing form ${selector}`); }
async function browserClick(cdp, selector) { ok(await cdp.evaluate(`(() => { const e = document.querySelector(${JSON.stringify(selector)}); if (!e) return false; e.click(); return true; })()`), `missing control ${selector}`); }
async function browserText(cdp, selector) { return cdp.evaluate(`document.querySelector(${JSON.stringify(selector)})?.textContent ?? ""`); }
async function browserInstallGameRenderProbe(cdp) {
  ok(await cdp.evaluate(`(() => {
    const app = document.querySelector('#app');
    const shell = document.querySelector('.game-shell');
    const canvas = document.querySelector('#game-canvas');
    const fireForm = document.querySelector('#fire-form');
    const chatForm = document.querySelector('#chat-form');
    const functionInput = document.querySelector('#function-input');
    if (!app || !shell || !canvas || !fireForm || !chatForm || !functionInput) return false;
    let replacements = 0;
    const observer = new MutationObserver(() => { replacements += 1; });
    observer.observe(app, {childList: true});
    window.__graphwarGameRenderProbe = {app, shell, canvas, fireForm, chatForm, functionInput, observer, get replacements() { return replacements; }};
    return true;
  })()`), "game render probe setup failed");
}
async function browserAssertGameStable(cdp, label) {
  ok(await cdp.evaluate(`(() => {
    const probe = window.__graphwarGameRenderProbe;
    if (!probe) return false;
    const same = probe.app === document.querySelector('#app')
      && probe.shell === document.querySelector('.game-shell')
      && probe.canvas === document.querySelector('#game-canvas')
      && probe.fireForm === document.querySelector('#fire-form')
      && probe.chatForm === document.querySelector('#chat-form')
      && probe.functionInput === document.querySelector('#function-input');
    probe.observer.disconnect();
    return same && probe.replacements === 0;
  })()`), `${label}: game DOM was replaced`);
}

async function browserRegister(browser, user) {
  const {cdp} = browser;
  await browserWait(cdp, "Boolean(document.querySelector('#register-form'))", "register screen");
  await browserSet(cdp, "#register-name", user.display_name);
  await browserSet(cdp, "#register-email", user.email);
  await browserSet(cdp, "#register-password", user.password);
  await browserSubmit(cdp, "#register-form");
  await browserWait(cdp, "Boolean(document.querySelector('#create-room-form'))", "lobby screen");
}
async function browserCreate(cdp, name, visibility) {
  await browserSet(cdp, "#room-name", name);
  await cdp.evaluate(`document.querySelector('#room-visibility').value = ${JSON.stringify(visibility)}; document.querySelector('#room-visibility').dispatchEvent(new Event('change',{bubbles:true}))`);
  await browserSubmit(cdp, "#create-room-form");
  await browserWait(cdp, "Boolean(document.querySelector('#room-title'))", "room screen");
}
async function browserLeave(cdp) {
  await browserClick(cdp, "#leave-room");
  await browserWait(cdp, "Boolean(document.querySelector('#create-room-form'))", "lobby after leave");
}

async function browserFlows() {
  const alpha = browserUser("alpha");
  const bravo = browserUser("bravo");
  const a = await launchBrowser(BASE);
  const b = await launchBrowser(BASE);
  try {
    await browserRegister(a, alpha);
    await browserRegister(b, bravo);
    const publicName = `Public E2E ${crypto.randomUUID()}`;
    await browserCreate(a.cdp, publicName, "public");
    const publicNameJs = JSON.stringify(publicName);
    await browserWait(b.cdp, `Boolean([...document.querySelectorAll('.room-list li')].find(li => li.querySelector('strong')?.textContent === ${publicNameJs}))`, "public room listing");
    ok(await b.cdp.evaluate(`(() => { const button = [...document.querySelectorAll('.room-list li')].find(li => li.querySelector('strong')?.textContent === ${publicNameJs})?.querySelector('.join-room'); if (!button) return false; button.click(); return true; })()`), "public room join control missing");
    await browserWait(b.cdp, "Boolean(document.querySelector('#room-title'))", "public roster guest");
    await browserWait(a.cdp, `(() => {
      const rows = [...document.querySelectorAll('.roster li')];
      return rows.length === 2
        && rows.some(row => row.querySelector('strong')?.textContent === ${JSON.stringify(alpha.display_name)})
        && rows.some(row => row.querySelector('strong')?.textContent === ${JSON.stringify(bravo.display_name)});
    })()`, "public roster synchronization");
    ok(await a.cdp.evaluate(`(() => {
      const input = document.querySelector('#chat-input');
      if (!input) return false;
      input.value = 'draft-chat';
      input.focus();
      input.setSelectionRange(2, 7, 'forward');
      return true;
    })()`), "missing draft chat input");
    await browserSet(b.cdp, ".player-soldiers", "1");
    await browserWait(a.cdp, `(() => {
      const input = document.querySelector('#chat-input');
      return input?.value === 'draft-chat'
        && document.activeElement === input
        && input.selectionStart === 2
        && input.selectionEnd === 7
        && input.selectionDirection === 'forward';
    })()`, "draft focus and caret preservation");
    ok(await a.cdp.evaluate(`(() => { const input = document.querySelector('#chat-input'); if (!input) return false; input.value = ''; return true; })()`), "missing draft chat input");
    await browserSet(a.cdp, ".player-soldiers", "1");
    await browserSet(b.cdp, ".player-team", "2");
    ok(await a.cdp.evaluate(`(() => {
      const input = document.querySelector('#chat-input');
      const form = document.querySelector('#chat-form');
      if (!input || !form) return false;
      input.value = 'public-chat';
      input.dispatchEvent(new Event('input', {bubbles: true}));
      form.requestSubmit();
      return true;
    })()`), "missing chat form");
    await browserWait(b.cdp, "document.body.textContent.includes('public-chat')", "chat delivery");
    await browserClick(a.cdp, "#ready-button");
    await browserClick(b.cdp, "#ready-button");
    await browserWait(a.cdp, "document.querySelector('#start-game')?.disabled === false", "start enabled");
    await browserClick(a.cdp, "#start-game");
    await browserWait(a.cdp, "Boolean(document.querySelector('#game-canvas'))", "public game owner", 20_000);
    await browserWait(b.cdp, "Boolean(document.querySelector('#game-canvas'))", "public game guest", 20_000);
    for (const cdp of [a.cdp, b.cdp]) {
      ok(await cdp.evaluate(`(() => {
        const canvas = document.querySelector('#game-canvas');
        const summary = document.querySelector('#battlefield-summary');
        const teams = [...document.querySelectorAll('.scoreboard .team-name')].map(node => node.textContent);
        return canvas?.getAttribute('aria-label') === 'Graphwar battlefield'
          && canvas?.getAttribute('aria-describedby') === summary?.id
          && summary?.textContent.includes('Team One')
          && summary?.textContent.includes('Team Two')
          && teams.includes('Team One')
          && teams.includes('Team Two');
      })()`), "battlefield accessibility semantics missing");
    }
    await a.cdp.command("Emulation.setDeviceMetricsOverride", {width: 320, height: 800, deviceScaleFactor: 2, mobile: false});
    await browserWait(a.cdp, `(() => {
      const canvas = document.querySelector('#game-canvas');
      return canvas && canvas.width === Math.round(canvas.getBoundingClientRect().width * 2)
        && canvas.height === Math.round(canvas.getBoundingClientRect().height * 2)
        && document.documentElement.scrollWidth <= 320;
    })()`, "responsive DPR canvas");
    for (const cdp of [a.cdp, b.cdp]) await browserInstallGameRenderProbe(cdp);
    const activeIsA = await a.cdp.evaluate("document.querySelector('#function-input')?.disabled === false");
    const active = activeIsA ? a.cdp : b.cdp;
    const inactive = activeIsA ? b.cdp : a.cdp;
    const previewExceptions = [];
    active.on("Runtime.exceptionThrown", ({exceptionDetails}) => previewExceptions.push(exceptionDetails.text ?? "browser exception"));
    await browserSet(active, "#function-input", "x+");
    await browserWait(active, "document.querySelector('#function-error')?.textContent === 'Invalid function'", "invalid preview error");
    await browserSet(active, "#function-input", "sin(x)");
    await browserWait(active, "document.querySelector('#function-error')?.textContent === ''", "valid preview recovery");
    ok(await active.evaluate(`(() => {
      const input = document.querySelector('#function-input');
      input?.focus();
      input?.setSelectionRange(1, 4, 'backward');
      window.dispatchEvent(new Event('resize'));
      return document.querySelector('label[for="function-input"]')?.textContent === 'Function'
        && document.querySelector('#game-canvas')?.getAttribute('aria-label') === 'Graphwar battlefield';
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
    await browserSubmit(active, "#fire-form");
    await browserWait(active, "document.querySelector('#turn-timer')?.textContent.includes('Resolving')", "authoritative shot", 20_000);
    await browserWait(inactive, "document.querySelector('#turn-timer')?.textContent.includes('Resolving')", "remote authoritative shot", 20_000);
    for (const cdp of [a.cdp, b.cdp]) await browserAssertGameStable(cdp, "shot update");
    await a.cdp.command("Page.reload");
    await browserWait(a.cdp, "Boolean(document.querySelector('#game-canvas'))", "state sync after refresh", 20_000);
    await browserExpireSession(a);
    await browserWait(a.cdp, "Boolean(document.querySelector('#login-form'))", "external session expiration", 20_000);
    await browserWait(
      a.cdp,
      "document.querySelector('.notices')?.textContent.includes('Session expired; sign in again')",
      "session expiration notice",
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

    const c = await launchBrowser(BASE);
    const d = await launchBrowser(BASE);
    try {
      await browserRegister(c, browserUser("private-owner"));
      await browserRegister(d, browserUser("private-guest"));
      const privateName = `Private E2E ${crypto.randomUUID()}`;
      await browserCreate(c.cdp, privateName, "private");
      const notice = await browserText(c.cdp, ".notices");
      const invite = notice.match(/Private room:\s*([0-9a-f-]{36})\s*·\s*invite:\s*([0-9a-f-]{36})/i);
      ok(invite, "private room invite not displayed");
      ok(
        !await d.cdp.evaluate(`[...document.querySelectorAll('.room-list strong')].some(node => node.textContent === ${JSON.stringify(privateName)})`),
        "private room leaked into lobby listing",
      );
      const privateProtocol = await registerAndLogin("private-protocol");
      const privateWs = await openWs(privateProtocol.cookie);
      privateWs.sendText(JSON.stringify({type: "hello", payload: {version: PROTOCOL_VERSION}}));
      await privateWs.next("private websocket hello");
      await privateWs.next("private websocket state sync");
      const privateRooms = JSON.parse((await privateWs.next("private websocket room list")).text);
      ok(
        privateRooms.type === "room_list"
          && !privateRooms.payload.rooms.some(room => room.id === invite[1]),
        "private room leaked into protocol room list",
      );
      privateWs.sendText(JSON.stringify({type: "join_room", payload: {room_id: invite[1], invite: null}}));
      const missingInvite = JSON.parse((await privateWs.next("missing private invite rejection")).text);
      ok(missingInvite.type === "error" && missingInvite.payload.code === "private", "missing invite was accepted");
      privateWs.close();
      await browserSet(d.cdp, "#private-room-id", invite[1]);
      await browserSet(d.cdp, "#invite-code", crypto.randomUUID());
      await browserSubmit(d.cdp, "#invite-room-form");
      await browserWait(d.cdp, "document.querySelector('.notices')?.textContent.includes('room is private')", "wrong private invite rejection");
      ok(await d.cdp.evaluate("Boolean(document.querySelector('#create-room-form'))"), "wrong invite left lobby");
      await browserSet(d.cdp, "#invite-code", invite[2]);
      await browserSubmit(d.cdp, "#invite-room-form");
      await browserWait(d.cdp, "Boolean(document.querySelector('#room-title'))", "private invite join");
      log("private room visibility and invite enforcement: pass");
      await browserLeave(d.cdp);
      await browserLeave(c.cdp);

      await browserCreate(c.cdp, "Bot E2E", "public");
      await browserClick(c.cdp, "#add-bot");
      await browserWait(c.cdp, "document.body.textContent.includes('Computer')", "bot slot");
      await browserClick(c.cdp, "#ready-button");
      await browserWait(c.cdp, "document.querySelector('#start-game')?.disabled === false", "bot start enabled");
      await browserClick(c.cdp, "#start-game");
      await browserWait(c.cdp, "Boolean(document.querySelector('#game-canvas'))", "bot game", 20_000);
      await browserWait(c.cdp, "document.querySelector('#function-input')?.disabled === false", "human bot-game turn", 20_000);
      await browserSet(c.cdp, "#function-input", "0");
      await browserSubmit(c.cdp, "#fire-form");
      await browserWait(c.cdp, "document.querySelector('#turn-timer')?.textContent.includes('Resolving')", "human shot before bot", 20_000);
      await browserWait(
        c.cdp,
        "document.querySelector('#function-input')?.disabled === false",
        "bot completed authoritative turn",
        30_000,
      );
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
