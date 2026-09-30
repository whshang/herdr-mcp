// page-assist.js — compatibility message entry for the shared Browser Page Kernel.
(function (global) {
  "use strict";

  if (global.__H2W_PAGE_ASSIST_LISTENER__) return;
  global.__H2W_PAGE_ASSIST_LISTENER__ = true;

  if (typeof chrome === "undefined" || !chrome.runtime?.onMessage) return;
  chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    if (msg?.type !== "h2w_page_assist") return false;
    const kernel = global.H2W_BROWSER_PAGE_KERNEL;
    const result = kernel?.handleAction
      ? kernel.handleAction(msg)
      : { ok: false, error: "browser_page_kernel_unavailable" };
    if (result && typeof result.then === "function") {
      result
        .then((value) => sendResponse(value))
        .catch(() => sendResponse({ ok: false, error: "browser_page_kernel_failed" }));
      return true;
    }
    sendResponse(result);
    return false;
  });
})(typeof globalThis !== "undefined" ? globalThis : window);
