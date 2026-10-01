import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import ts from "typescript";
import { createInstance } from "i18next";

const root = new URL("../", import.meta.url);
const read = (file) => fs.readFileSync(new URL(file, root), "utf8");
const en = JSON.parse(read("src/i18n/locales/en.json"));
const zh = JSON.parse(read("src/i18n/locales/zh-CN.json"));
const languages = ["en", "zh-CN", "zh-TW", "ja", "ko", "es", "fr", "de", "pt-BR", "ru"];
const languageModule = ts.transpileModule(read("src/i18n/languages.ts"), { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2023 } }).outputText;
const { LANGUAGE_OPTIONS, resolveSystemLanguage } = await import(`data:text/javascript;base64,${Buffer.from(languageModule).toString("base64")}`);
const native = JSON.parse(read("src-tauri/src/locales.json"));
const placeholders = (text) => [...text.matchAll(/\{\{([^}]+)\}\}/g)].map((m) => m[1]).sort();

test("all ten frontend languages have identical keys and interpolation parameters", () => {
  assert.deepEqual(LANGUAGE_OPTIONS.map(({ value }) => value).sort(), [...languages].sort());
  for (const language of languages) {
    const messages = JSON.parse(read(`src/i18n/locales/${language}.json`));
    assert.deepEqual(Object.keys(en).sort(), Object.keys(messages).sort(), language);
    for (const key of Object.keys(en)) {
      assert.ok(messages[key].trim(), `Empty message: ${language}.${key}`);
      assert.deepEqual(placeholders(en[key]), placeholders(messages[key]), `${language}.${key}`);
    }
  }
});

test("system language detection handles scripts, regions and unsupported languages", () => {
  for (const [input, expected] of [
    [null, "en"], ["", "en"], ["id-ID", "en"], ["en-US", "en"],
    ["zh", "zh-CN"], ["zh_SG.UTF-8", "zh-CN"], ["zh-Hans-TW", "zh-CN"],
    ["ZH_tw", "zh-TW"], ["zh-HK", "zh-TW"], ["zh-MO", "zh-TW"], ["zh-Hant-CN", "zh-TW"],
    ["ja-JP", "ja"], ["ko_KR.UTF-8", "ko"], ["es-MX", "es"],
    ["fr-CA", "fr"], ["de-AT", "de"], ["pt-PT", "pt-BR"], ["pt_BR", "pt-BR"], ["ru-RU", "ru"],
  ]) assert.equal(resolveSystemLanguage(input), expected, String(input));
});

test("translations preserve copyable headers, proxy protocols and cancellation messages", () => {
  for (const language of languages) {
    const messages = JSON.parse(read(`src/i18n/locales/${language}.json`));
    const headers = messages.oneHeaderPerLineRefererHttpsExampleComOriginHttps;
    for (const example of ["referer:https://example.com", "origin:https://example.com"]) {
      assert.ok(headers.includes(example), `${language}: ${example}`);
    }
    const recordingHeaders = messages.onePerLineInNameValueFormatForExampleReferer;
    for (const example of ["Referer:https://example.com", "User-Agent:Mozilla/5.0"]) {
      assert.ok(recordingHeaders.includes(example), `${language}: ${example}`);
    }
    const proxyKey = Object.keys(en).find((key) => en[key].includes("socks5://"));
    for (const protocol of ["http://", "https://", "socks5://"]) {
      assert.ok(messages[proxyKey].includes(protocol), `${language}: ${protocol}`);
    }
    const cancellation = Object.values(native).find((entry) => entry.en === "Preview cancelled");
    assert.equal(cancellation[language], messages.previewCancelled, language);
  }
});

test("missing translations fall back to English and user content is preserved", async () => {
  const instance = createInstance();
  await instance.init({
    lng: "zh-CN", fallbackLng: "en",
    resources: { en: { translation: en }, "zh-CN": { translation: { playing: zh.playing } } },
    interpolation: { escapeValue: false },
  });
  assert.equal(instance.t("settings"), en.settings);
  const filename = "用户视频 <sample> {{name}}.mp4";
  assert.equal(instance.t("playing", { value0: filename }), `播放中 - ${filename}`);
  await instance.changeLanguage("en");
  assert.equal(instance.t("playing", { value0: filename }), `Playing - ${filename}`);
});

test("native resources have all languages and matching parameter counts", () => {
  for (const [key, message] of Object.entries(native)) {
    for (const language of languages) {
      assert.ok(message[language]?.trim(), `${language}.${key}`);
      assert.equal(message.en.split("{}").length, message[language].split("{}").length, `${language}.${key}`);
    }
  }
  for (const filename of fs.readdirSync(new URL("src-tauri/src/", root)).filter((f) => f.endsWith(".rs"))) {
    for (const match of read(`src-tauri/src/${filename}`).matchAll(/(?:i18n::tr|localized!)\(\s*"([^"]+)"/g)) {
      assert.ok(native[match[1]], `Missing native key ${match[1]} in ${filename}`);
    }
  }
});

test("desktop UI strings are translated except native language names", () => {
  const allowed = new Set(["语言 / Language", ...LANGUAGE_OPTIONS.map(({ label }) => label)]);
  function inspect(directory) {
    for (const entry of fs.readdirSync(new URL(directory + "/", root), { withFileTypes: true })) {
      const file = path.posix.join(directory, entry.name);
      if (entry.isDirectory()) { inspect(file); continue; }
      if (!/\.tsx?$/.test(file)) continue;
      const source = ts.createSourceFile(file, read(file), ts.ScriptTarget.Latest, true);
      function visit(node) {
        if (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node) || ts.isJsxText(node)) {
          assert.ok(!/\p{Script=Han}/u.test(node.text) || allowed.has(node.text.trim()), `Untranslated UI string in ${file}: ${node.text}`);
        }
        ts.forEachChild(node, visit);
      }
      visit(source);
    }
  }
  inspect("src");
});
