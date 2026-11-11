import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const source = fs.readFileSync(new URL("../src/hooks/useDownloads.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;

test("search labels exist in all ten languages with matching placeholders", () => {
  const keys = ["searchDownloadFilename", "searchByFilename", "matchingDownloadCount", "noMatchingDownloads", "searchingDownloads", "failedToLoadDownloadHistory"];
  for (const locale of ["en", "zh-CN", "zh-TW", "ja", "ko", "de", "es", "fr", "pt-BR", "ru"]) {
    const messages = JSON.parse(fs.readFileSync(new URL(`../src/i18n/locales/${locale}.json`, import.meta.url), "utf8"));
    for (const key of keys) {
      assert.ok(messages[key]?.trim(), `${locale}.${key}`);
      const placeholders = [...messages[key].matchAll(/\{\{([^}]+)\}\}/g)].map((match) => match[1]);
      assert.deepEqual(placeholders, ["matchingDownloadCount", "failedToLoadDownloadHistory"].includes(key) ? ["value0"] : [], `${locale}.${key}`);
    }
  }
});

// Run the actual hook with controlled effects, requests and timers, without native Tauri/UI dependencies.
function createHarness() {
  const slots = [];
  const timers = new Map();
  const requests = [];
  const errors = [];
  let cursor = 0;
  let now = 0;
  let timerId = 0;
  let dirty = false;
  let pageSize = 50;
  let effects = [];
  let listener;
  let clearCalls = 0;
  let result;
  const changed = (before, after) => !before || before.some((value, i) => !Object.is(value, after[i]));
  const react = {
    useState(initial) {
      const index = cursor++;
      slots[index] ??= { value: typeof initial === "function" ? initial() : initial };
      return [slots[index].value, (next) => {
        slots[index].value = typeof next === "function" ? next(slots[index].value) : next;
        dirty = true;
      }];
    },
    useRef(value) {
      const index = cursor++;
      return slots[index] ??= { current: value };
    },
    useCallback(callback, deps) {
      const index = cursor++;
      if (changed(slots[index]?.deps, deps)) slots[index] = { value: callback, deps };
      return slots[index].value;
    },
    useEffect(callback, deps) {
      const index = cursor++;
      if (changed(slots[index]?.deps, deps)) {
        const previous = slots[index];
        slots[index] = { deps };
        effects.push(() => {
          previous?.cleanup?.();
          slots[index].cleanup = callback();
        });
      }
    },
  };
  const api = {
    getDownloadCounts: async () => ({ active_count: 0, history_count: 128 }),
    getDownloadsPage(group, page, size, keyword) {
      if (group === "active") return Promise.resolve({ items: [], total: 0, page, page_size: size });
      return new Promise((resolve, reject) => requests.push({ page, size, keyword, resolve, reject }));
    },
    removeDownload: async () => {},
    clearHistoryDownloads: async () => { clearCalls += 1; },
  };
  const modules = {
    react,
    "../i18n": { t: (key) => key, useTranslation() {} },
    "../services/api": api,
    antd: { message: { error: (value) => errors.push(value), success() {} }, Modal: {} },
    "@tauri-apps/api/event": { listen: async (_, callback) => {
      listener = callback;
      return () => { if (listener === callback) listener = undefined; };
    } },
  };
  const exports = {};
  vm.runInNewContext(compiled, {
    exports,
    require: (name) => { assert.ok(modules[name], name); return modules[name]; },
    console: { error() {} },
    setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback, due: now + delay }); return id; },
    clearTimeout: (id) => timers.delete(id),
  });
  const render = () => {
    cursor = 0;
    dirty = false;
    effects = [];
    result = exports.useDownloads(pageSize);
    effects.forEach((effect) => effect());
  };
  const flush = async () => {
    for (let i = 0; i < 12; i += 1) {
      await Promise.resolve();
      if (dirty) render();
    }
  };
  render();
  return {
    requests, errors, flush,
    get current() { return result; },
    get clearCalls() { return clearCalls; },
    async advance(ms) {
      now += ms;
      for (const [id, timer] of [...timers]) {
        if (timer.due <= now) { timers.delete(id); timer.callback(); }
      }
      await flush();
    },
    async answer(request, name, total = 1, page = request.page) {
      request.resolve({ items: name ? [{ id: name, filename: name }] : [], total, page, page_size: request.size });
      await flush();
    },
    async resize(size) { pageSize = size; render(); await flush(); },
    async completeTask() { listener({ payload: { id: "finished", group: "history" } }); await flush(); },
    unmount() { for (const slot of slots) slot?.cleanup?.(); },
  };
}

test("filename input debounces and clearing immediately restores the unfiltered first page", async () => {
  const h = createHarness();
  await h.flush();
  h.current.searchHistory("旅");
  await h.advance(200);
  h.current.searchHistory("  旅行  ");
  await h.advance(299);
  assert.equal(h.requests.length, 1);
  await h.advance(1);
  assert.equal(h.requests[1].keyword, "旅行");
  assert.equal(h.requests[1].page, 1);
  await h.answer(h.requests[1], "旅行.mp4", 3);
  assert.equal(h.current.completedTotal, 3);
  h.current.searchHistory("");
  await h.advance(0);
  assert.equal(h.requests[2].keyword, "");
  assert.equal(h.requests[2].page, 1);
  h.unmount();
});

test("old responses cannot overwrite new keywords, including during debounce", async () => {
  const h = createHarness();
  await h.flush();
  h.current.searchHistory("old");
  await h.answer(h.requests[0], "unfiltered.mp4");
  assert.equal(h.current.completed.length, 0);
  assert.equal(h.current.loadingHistory, true);
  await h.advance(300);
  h.current.searchHistory("new");
  await h.advance(300);
  await h.answer(h.requests[2], "new.mp4");
  await h.answer(h.requests[1], "old.mp4");
  assert.equal(h.current.completed[0].filename, "new.mp4");
  assert.equal(h.current.loadingHistory, false);
  h.unmount();
});

test("pagination uses the keyword and only the newest page response wins", async () => {
  const h = createHarness();
  await h.flush();
  h.current.searchHistory("trip");
  await h.advance(300);
  await h.answer(h.requests[1], "trip-1", 150);
  const second = h.current.refreshHistory(2);
  const third = h.current.refreshHistory(3);
  assert.equal(h.requests[2].keyword, "trip");
  assert.equal(h.requests[3].keyword, "trip");
  await h.answer(h.requests[3], "trip-3", 150);
  await h.answer(h.requests[2], "trip-2", 150);
  await Promise.all([second, third]);
  assert.equal(h.current.completedPage, 3);
  assert.equal(h.current.completed[0].filename, "trip-3");
  h.unmount();
});

test("task completion and removal preserve search, and filtered views cannot clear all history", async () => {
  const h = createHarness();
  await h.flush();
  h.current.searchHistory("trip");
  await h.advance(300);
  await h.answer(h.requests[1], "trip-1", 51);
  void h.current.refreshHistory(2);
  await h.answer(h.requests[2], "trip-51", 51);
  await h.completeTask();
  assert.equal(h.requests[3].keyword, "trip");
  assert.equal(h.requests[3].page, 2);
  await h.answer(h.requests[3], "trip-51", 51);
  const removal = h.current.remove("trip-51", false);
  await h.flush();
  assert.equal(h.requests[4].keyword, "trip");
  await h.answer(h.requests[4], "trip-1", 50, 1);
  await removal;
  assert.equal(h.current.completedPage, 1);
  await h.current.clearCompleted();
  assert.equal(h.clearCalls, 0);
  h.unmount();
});

test("page-size changes keep the keyword and invalidate previous requests", async () => {
  const h = createHarness();
  await h.flush();
  h.current.searchHistory("trip");
  await h.advance(300);
  await h.resize(100);
  assert.equal(h.requests[2].keyword, "trip");
  assert.equal(h.requests[2].size, 100);
  assert.equal(h.requests[2].page, 1);
  await h.answer(h.requests[2], "new-size", 100);
  h.requests[1].reject(new Error("old request failed"));
  await h.flush();
  assert.equal(h.errors.length, 0);
  assert.equal(h.current.completedPageSize, 100);
  h.current.searchHistory("pending");
  h.unmount();
  await h.advance(300);
  assert.equal(h.requests.length, 3);
});
