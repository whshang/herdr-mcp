import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const I18N_URL = pathToFileURL(path.join(ROOT, "extension/i18n.js"));

async function importFresh(tag) {
  return import(`${I18N_URL.href}?test=${encodeURIComponent(tag)}-${Date.now()}-${Math.random()}`);
}

async function localeCatalog(code) {
  return JSON.parse(await readFile(path.join(ROOT, `extension/locales/${code}.json`), "utf8"));
}

test("extension locale catalogs keep complete key and non-empty value parity", async () => {
  const catalogs = Object.fromEntries(await Promise.all(
    ["en", "zh", "ja"].map(async (code) => [code, await localeCatalog(code)]),
  ));
  const canonical = Object.keys(catalogs.en).sort();
  assert.ok(canonical.length > 400, "canonical locale should cover the full extension surface");
  for (const code of ["zh", "ja"]) {
    assert.deepEqual(Object.keys(catalogs[code]).sort(), canonical, `${code} keys must match English`);
  }
  for (const [code, catalog] of Object.entries(catalogs)) {
    for (const key of canonical) {
      assert.equal(typeof catalog[key], "string", `${code}.${key} must be a string`);
      assert.ok(catalog[key].trim().length > 0, `${code}.${key} must not be empty`);
    }
  }
});

test("system locale detection normalizes Chinese and Japanese variants and falls back to English", async () => {
  const originalChrome = globalThis.chrome;
  try {
    for (const [raw, expected] of [
      ["zh-CN", "zh"],
      ["zh_Hant_TW", "zh"],
      ["ja-JP", "ja"],
      ["en-GB", "en"],
      ["de-DE", "en"],
      ["", "en"],
    ]) {
      globalThis.chrome = { i18n: { getUILanguage: () => raw } };
      const { detectSystemLocale } = await importFresh(`detect-${raw || "empty"}`);
      assert.equal(detectSystemLocale(), expected, raw || "empty locale");
    }
  } finally {
    globalThis.chrome = originalChrome;
  }
});

test("first-run detection persists a supported locale and initialized invalid state fails closed to English", async () => {
  const originalChrome = globalThis.chrome;
  const originalFetch = globalThis.fetch;
  try {
    const writes = [];
    let stored = {};
    globalThis.chrome = {
      i18n: { getUILanguage: () => "ja-JP" },
      runtime: { getURL: (value) => `chrome-extension://test/${value}` },
      storage: {
        local: {
          get: async () => stored,
          set: async (value) => { writes.push(value); stored = { ...stored, ...value }; },
        },
      },
    };
    globalThis.fetch = async (url) => ({
      json: async () => localeCatalog(url.match(/locales\/(en|zh|ja)\.json$/)?.[1] || "en"),
    });

    let i18n = await importFresh("first-run-ja");
    assert.equal(await i18n.detectOrLoadLocale(), "ja");
    assert.deepEqual(writes.at(-1), { uiLocale: "ja", uiLocaleInitialized: true });

    stored = { uiLocale: "unsupported", uiLocaleInitialized: true };
    writes.length = 0;
    i18n = await importFresh("invalid-stored");
    assert.equal(await i18n.detectOrLoadLocale(), "en");
    assert.equal(writes.length, 0, "invalid initialized state must not silently rewrite user preference");
  } finally {
    globalThis.chrome = originalChrome;
    globalThis.fetch = originalFetch;
  }
});
