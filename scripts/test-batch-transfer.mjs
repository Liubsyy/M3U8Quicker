import assert from "node:assert/strict";
import fs from "node:fs";
import vm from "node:vm";
import test from "node:test";
import { createHmac, webcrypto } from "node:crypto";
import ts from "typescript";

const read = (path) => fs.readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const payload = {
  items: Array.from({ length: 500 }, (_, i) => ({ url: `https://example.com/${i}.m3u8?token=${"a".repeat(400)}`, title: `视频 ${i}`, file_type: "hls" })),
  extra_headers: "referer:https://example.com\nuser-agent:test",
};

function backgroundHarness(platform, fetch, stored = {}, timers = {}) {
  const context = vm.createContext({ TextEncoder, Uint8Array, crypto: webcrypto, AbortController, setTimeout, clearTimeout, fetch, ...timers });
  if (platform === "firefox") {
    context.browser = { storage: { local: {
      get: async () => ({ ...stored }),
      set: async (values) => { Object.assign(stored, values); },
    } } };
  } else {
    context.chrome = { runtime: {}, storage: { local: {
      get: (_, callback) => callback({ ...stored }),
      set: (values, callback) => { Object.assign(stored, values); callback(); },
    } } };
  }
  vm.runInContext(read(`browser-extension/${platform}/batch-transfer.js`), context);
  return context.m3u8quickerTransferBatchAttempt;
}

const credentials = { id: "a".repeat(32), secret: "b".repeat(64) };
function signedProbe(url, status = "ready") {
  const proof = createHmac("sha256", credentials.secret).update(
    `m3u8quicker-batch-v1:${credentials.id}:${url.searchParams.get("challenge")}:${status}`,
  ).digest("hex");
  return new Response(JSON.stringify({ proof, status }));
}

for (const platform of ["chrome", "firefox"]) {
  test(`${platform}: caches authenticated port across attempts and background restarts`, async () => {
    const stored = {};
    const probes = [];
    const fetch = async (address, options) => {
      const url = new URL(address);
      if (options.method === "POST") return new Response("", { status: 200 });
      probes.push(Number(url.port));
      return url.port === "49325" ? signedProbe(url) : new Response("", { status: 404 });
    };
    const attempt = backgroundHarness(platform, fetch, stored);
    assert.equal((await attempt({ ...credentials, payload })).ok, true);
    assert.equal(stored.m3u8quickerBatchPort, 49325);
    probes.length = 0;
    assert.equal((await attempt({ ...credentials, payload })).ok, true);
    assert.deepEqual(probes, [49325]);
    probes.length = 0;
    const restarted = backgroundHarness(platform, fetch, stored);
    assert.equal((await restarted({ ...credentials, payload })).ok, true);
    assert.deepEqual(probes, [49325]);
  });

  test(`${platform}: invalid cached identity falls back and uploads before slow probes finish`, { timeout: 2500 }, async () => {
    const stored = { m3u8quickerBatchPort: 49320 };
    const probes = [];
    const slowSignals = [];
    let posts = 0;
    const fetch = async (address, options) => {
      const url = new URL(address);
      if (options.method === "POST") {
        posts += 1;
        assert.equal(url.port, "49326", "never send the payload to the stale port");
        assert.equal(slowSignals.length, 8);
        assert.ok(slowSignals.every((signal) => signal.aborted), "cancel slow probes as soon as a port verifies");
        return new Response("", { status: 200 });
      }
      probes.push(Number(url.port));
      if (url.port === "49320") return new Response(JSON.stringify({ status: "ready", proof: "0".repeat(64) }));
      if (url.port === "49326") return signedProbe(url);
      slowSignals.push(options.signal);
      return new Promise((_, reject) => options.signal.addEventListener("abort", () => reject(new Error("aborted")), { once: true }));
    };
    const attempt = backgroundHarness(platform, fetch, stored, {
      // Slow ports cannot complete via timeout: only winner cancellation can release them.
      setTimeout: (callback, delay) => delay === 900 ? null : setTimeout(callback, delay),
    });
    assert.equal((await attempt({ ...credentials, payload })).ok, true);
    assert.equal(probes[0], 49320);
    assert.deepEqual(probes.slice(1).sort(), Array.from({ length: 10 }, (_, i) => 49320 + i));
    assert.equal(posts, 1);
    assert.equal(stored.m3u8quickerBatchPort, 49326);
  });

  test(`${platform}: unavailable ports remain retryable and failed uploads invalidate cache`, async () => {
    const stored = { m3u8quickerBatchPort: 49321 };
    const probes = [];
    let online = false;
    const attempt = backgroundHarness(platform, async (address, options) => {
      const url = new URL(address);
      if (options.method === "POST") throw new Error("connection lost");
      probes.push(Number(url.port));
      if (online && url.port === "49327") return signedProbe(url);
      throw new Error("connection refused");
    }, stored);
    assert.equal((await attempt({ ...credentials, payload })).retryable, true);
    assert.equal(probes.length, 11);
    assert.equal(stored.m3u8quickerBatchPort, null);
    probes.length = 0;
    online = true;
    assert.equal((await attempt({ ...credentials, payload })).retryable, true);
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(stored.m3u8quickerBatchPort, null);
    probes.length = 0;
    await attempt({ ...credentials, payload });
    assert.equal(probes.length, 10, "rediscover after an upload connection failure");
  });

  test(`${platform}: authenticate fallback port before sending a large payload, then acknowledge retry`, async () => {
    const id = "a".repeat(32), secret = "b".repeat(64);
    let delivered = false;
    const posts = [];
    const context = vm.createContext({ TextEncoder, Uint8Array, crypto: webcrypto, AbortController, setTimeout, clearTimeout,
      fetch: async (address, options) => {
        const url = new URL(address);
        if (options.method === "POST") {
          posts.push({ url, options });
          delivered = true;
          return new Response("", { status: 202 });
        }
        if (url.port !== "49323") return new Response(JSON.stringify({ proof: "0".repeat(64), status: "ready" }));
        const status = delivered ? "delivered" : "ready";
        const proof = createHmac("sha256", secret).update(`m3u8quicker-batch-v1:${id}:${url.searchParams.get("challenge")}:${status}`).digest("hex");
        return new Response(JSON.stringify({ proof, status }));
      },
    });
    vm.runInContext(read(`browser-extension/${platform}/batch-transfer.js`), context);
    const attempt = context.m3u8quickerTransferBatchAttempt;
    assert.equal((await attempt({ id, secret, payload })).retryable, true);
    assert.equal(posts.length, 1);
    assert.equal(posts[0].url.port, "49323");
    assert.equal(posts[0].options.headers.Authorization, `Bearer ${secret}`);
    assert.deepEqual(JSON.parse(posts[0].options.body), payload);
    assert.equal((await attempt({ id, secret, payload })).ok, true);
    assert.equal(posts.length, 1, "lost acknowledgement must not upload again after delivery");
    assert.equal((await attempt({ id, secret, payload: { items: [{ url: "x".repeat(16 * 1024 * 1024) }] } })).error, "tooLarge");
  });

  test(`${platform}: short wake link, timeout retry ID reuse, and fresh ID after success`, async () => {
    const source = read(`browser-extension/${platform}/content.js`);
    const implementation = source.slice(source.indexOf("  let batchTransferCache = null;"), source.indexOf("  function buildTargetsWithUniqueNames"));
    let now = 0, succeed = false;
    const sent = [];
    const context = vm.createContext({
      URLSearchParams, TextEncoder, Uint8Array, crypto: webcrypto,
      APP_BATCH_DEEP_LINK_BASE_URL: "m3u8quicker://batch-transfer",
      window: { location: { href: "" } }, buildExtraHeaders: () => payload.extra_headers,
      Date: { now: () => now },
      setTimeout: (callback, delay) => { if (delay === 700) { now += delay; queueMicrotask(callback); } return 1; },
      clearTimeout: () => {},
      chrome: { runtime: { sendMessage: (message, callback) => { sent.push(message); callback(succeed ? { ok: true } : { retryable: true }); } } },
    });
    if (platform === "firefox") context.browser = { runtime: { sendMessage: async (message) => { sent.push(message); return succeed ? { ok: true } : { retryable: true }; } } };
    vm.runInContext(implementation, context);
    const items = payload.items.map((item) => ({ url: item.url, batchTitle: item.title, fileType: item.file_type }));
    await assert.rejects(context.openBatchDownloader(items), /timeout/);
    const first = new URL(context.window.location.href);
    assert.ok(first.href.length < 250);
    assert.equal(first.searchParams.has("items"), false);
    assert.equal(JSON.stringify(sent[0].payload), JSON.stringify(payload));
    succeed = true;
    await context.openBatchDownloader(items);
    const retry = new URL(context.window.location.href);
    assert.equal(retry.searchParams.get("id"), first.searchParams.get("id"));
    assert.notEqual(retry.searchParams.get("attempt"), first.searchParams.get("attempt"));
    await context.openBatchDownloader(items);
    assert.notEqual(new URL(context.window.location.href).searchParams.get("id"), first.searchParams.get("id"));
  });

  test(`${platform}: failure keeps selection and panel; success closes it`, async () => {
    const source = read(`browser-extension/${platform}/content.js`);
    const begin = source.indexOf('    batchButton.addEventListener("click", async () => {');
    const end = source.indexOf('    const languageSwitch', begin);
    let handler, closed = false, succeed = false;
    const selectedUrls = new Set(["one", "two"]);
    const context = vm.createContext({
      selectedUrls, batchButton: { addEventListener: (_, callback) => { handler = callback; } },
      batchStatus: { textContent: "" }, panelItems: [{ selectionKey: "one" }, { selectionKey: "two" }],
      panel: { remove: () => { closed = true; } }, updateBatchButtonState: () => {}, t: (key) => key,
      openBatchDownloader: async (items) => { assert.equal(items.length, 2); if (!succeed) throw new Error("timeout"); },
    });
    vm.runInContext('let batchSending = false; let batchErrorKey = "";\n' + source.slice(begin, end), context);
    await handler();
    assert.equal(closed, false);
    assert.equal(selectedUrls.size, 2);
    assert.equal(context.batchStatus.textContent, "batchTimeout");
    succeed = true;
    await handler();
    assert.equal(closed, true);
  });
}

test("desktop accepts old batch links and recognizes short transfer links separately", () => {
  const source = read("src/App.tsx");
  const ast = ts.createSourceFile("App.tsx", source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const functions = ast.statements.filter((node) => ts.isFunctionDeclaration(node) && ["parseBatchDownloadDraft", "parseBatchTransferLink"].includes(node.name?.text));
  const js = ts.transpileModule(functions.map((node) => node.getText(ast)).join("\n"), { compilerOptions: { target: ts.ScriptTarget.ES2022 } }).outputText;
  const context = vm.createContext({ URL, console, parseFileType: (value) => value || undefined, normalizeDownloadFilename: (value) => value.trim() });
  vm.runInContext(js, context);
  const query = new URLSearchParams({ items: "https://example.com/1.m3u8\nhttps://example.com/2.mp4", titles: JSON.stringify(["中文一", "中文二"]), file_types: "hls\nmp4", extra_headers: "referer:https://example.com" });
  const old = context.parseBatchDownloadDraft(`m3u8quicker://batch-download?${query}`);
  assert.equal(old.rawInput, query.get("items"));
  assert.equal(old.filenames.join(","), "中文一,中文二");
  assert.equal(old.fileTypes.join(","), "hls,mp4");
  assert.equal(old.extraHeaders, query.get("extra_headers"));
  const short = `m3u8quicker://batch-transfer/?id=${"a".repeat(32)}&secret=${"b".repeat(64)}`;
  assert.equal(context.parseBatchTransferLink(short).id, "a".repeat(32));
  assert.equal(context.parseBatchDownloadDraft(short), null);
  assert.equal(context.parseBatchTransferLink(short.replace("m3u8quicker:", "https:")), null);
  assert.equal(context.parseBatchTransferLink("m3u8quicker://batch-transfer?id=bad"), null);
});

test("both browsers use the same transport implementation", () => {
  assert.equal(read("browser-extension/chrome/batch-transfer.js"), read("browser-extension/firefox/batch-transfer.js"));
});
