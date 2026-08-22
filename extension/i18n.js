// i18n.js — en / zh / ja (align with herdr). First install: detect system UI language; default en.
const SUPPORTED = ["en", "zh", "ja"];
const catalogs = {
  en: null,
  zh: null,
  ja: null,
};

let locale = "en";
let readyResolve;
export const localeReady = new Promise((r) => { readyResolve = r; });

export function detectSystemLocale() {
  let raw = "en";
  try {
    if (typeof chrome !== "undefined" && chrome.i18n?.getUILanguage) {
      raw = chrome.i18n.getUILanguage();
    } else if (typeof navigator !== "undefined" && navigator.language) {
      raw = navigator.language;
    }
  } catch (_) { /* ignore */ }
  const lower = String(raw || "en").toLowerCase().replace("_", "-");
  if (lower.startsWith("zh")) return "zh";
  if (lower.startsWith("ja")) return "ja";
  return "en";
}

async function loadCatalog(code) {
  if (catalogs[code]) return catalogs[code];
  const url = chrome.runtime.getURL(`locales/${code}.json`);
  const resp = await fetch(url);
  catalogs[code] = await resp.json();
  return catalogs[code];
}

export async function detectOrLoadLocale() {
  let stored = {};
  try {
    stored = await chrome.storage.local.get(["uiLocale", "uiLocaleInitialized"]);
  } catch (_) { /* ignore */ }
  if (stored.uiLocale && SUPPORTED.includes(stored.uiLocale)) {
    locale = stored.uiLocale;
  } else if (!stored.uiLocaleInitialized) {
    locale = detectSystemLocale();
    try {
      await chrome.storage.local.set({ uiLocale: locale, uiLocaleInitialized: true });
    } catch (_) { /* ignore */ }
  } else {
    locale = "en";
  }
  await loadCatalog(locale);
  readyResolve?.(locale);
  return locale;
}

export async function setLocale(code) {
  if (!SUPPORTED.includes(code)) code = "en";
  locale = code;
  await loadCatalog(locale);
  try {
    await chrome.storage.local.set({ uiLocale: locale, uiLocaleInitialized: true });
  } catch (_) { /* ignore */ }
  return locale;
}

export function getLocale() {
  return locale;
}

export function t(key, vars) {
  const cat = catalogs[locale] || catalogs.en || {};
  let s = cat[key];
  if (s == null && catalogs.en) s = catalogs.en[key];
  if (s == null) s = key;
  if (vars && typeof s === "string") {
    for (const [k, v] of Object.entries(vars)) {
      s = s.replaceAll(`{${k}}`, String(v));
    }
  }
  return s;
}

export function onLocaleReady(cb) {
  localeReady.then(() => cb(locale));
}

export { SUPPORTED };
