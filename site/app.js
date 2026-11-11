(() => {
  const root = document.documentElement;
  const body = document.body;
  const themeKey = "herdr-docs-theme";
  const themeButton = document.querySelector("[data-theme-toggle]");
  const themeIcon = document.querySelector("[data-theme-icon]");

  const i18nNode = document.querySelector("#search-i18n");
  let i18n = {};
  if (i18nNode) {
    try {
      i18n = JSON.parse(i18nNode.textContent || "{}");
    } catch {
      i18n = {};
    }
  }

  function uiString(key, fallback) {
    return typeof i18n[key] === "string" && i18n[key] ? i18n[key] : fallback;
  }

  function applyTheme(theme) {
    root.dataset.theme = theme;
    if (themeIcon) {
      themeIcon.textContent = theme === "dark" ? "☀" : "☾";
    }
    if (themeButton) {
      themeButton.setAttribute(
        "aria-label",
        theme === "dark"
          ? uiString("themeToLight", "Switch to light theme")
          : uiString("themeToDark", "Switch to dark theme")
      );
    }
  }

  let savedTheme = null;
  try {
    savedTheme = localStorage.getItem(themeKey);
  } catch {
    savedTheme = null;
  }
  const prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
  const initialTheme = savedTheme === "dark" || savedTheme === "light" ? savedTheme : (prefersDark ? "dark" : "light");
  applyTheme(initialTheme);

  themeButton?.addEventListener("click", () => {
    const next = root.dataset.theme === "dark" ? "light" : "dark";
    try {
      localStorage.setItem(themeKey, next);
    } catch {
      /* storage unavailable */
    }
    applyTheme(next);
  });

  const navToggle = document.querySelector("[data-nav-toggle]");
  const sidebar = document.querySelector("[data-sidebar]");
  const overlay = document.querySelector("[data-nav-overlay]");

  function setDrawer(open) {
    if (!sidebar || !navToggle) return;
    body.classList.toggle("nav-open", open);
    navToggle.setAttribute("aria-expanded", String(open));
    navToggle.setAttribute(
      "aria-label",
      open
        ? uiString("closeNav", "Close documentation navigation")
        : uiString("openNav", "Open documentation navigation")
    );
    if (overlay) overlay.hidden = !open;
  }

  navToggle?.addEventListener("click", () => setDrawer(!body.classList.contains("nav-open")));
  overlay?.addEventListener("click", () => setDrawer(false));
  sidebar?.addEventListener("click", (event) => {
    if (event.target instanceof HTMLAnchorElement && window.matchMedia("(max-width: 1023px)").matches) {
      setDrawer(false);
    }
  });

  document.querySelectorAll("[data-lang-switch] a[data-locale]").forEach((link) => {
    link.addEventListener("click", () => {
      try {
        localStorage.setItem("herdr-docs-lang", link.dataset.locale);
      } catch {
        /* storage unavailable */
      }
    });
  });

  // Copy buttons on code blocks and prompts.
  document.querySelectorAll("[data-copy-code]").forEach((button) => {
    const defaultLabel = button.textContent || uiString("copy", "Copy");
    button.addEventListener("click", async () => {
      const pre = button.closest("pre");
      if (!pre) return;
      const codeEl = pre.querySelector("code");
      const source = (codeEl ? codeEl.textContent : pre.textContent || "").trim();
      try {
        await navigator.clipboard.writeText(source);
        button.dataset.copied = "true";
        button.textContent = `✓ ${uiString("copied", "Copied")}`;
        window.setTimeout(() => {
          delete button.dataset.copied;
          button.textContent = uiString("copy", defaultLabel);
        }, 1500);
      } catch {
        /* clipboard unavailable */
      }
    });
  });

  // Command-palette search dialog with keyboard arrow navigation.
  const dialog = document.querySelector("[data-search-dialog]");
  const searchInput = document.querySelector("[data-search-input]");
  const results = document.querySelector("[data-search-results]");
  const searchData = document.querySelector("#search-index");
  let index = [];
  if (searchData) {
    try {
      index = JSON.parse(searchData.textContent || "[]");
    } catch {
      index = [];
    }
  }

  let activeResultIdx = -1;

  function closeSearch() {
    if (dialog instanceof HTMLDialogElement && dialog.open) dialog.close();
  }

  function openSearch() {
    if (!(dialog instanceof HTMLDialogElement)) return;
    if (!dialog.open) dialog.showModal();
    window.setTimeout(() => searchInput?.focus(), 0);
  }

  function escapeHtml(value) {
    return String(value)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;");
  }

  function syncActiveResult() {
    if (!results) return;
    const links = [...results.querySelectorAll("li a")];
    links.forEach((el, i) => {
      el.classList.toggle("is-selected", i === activeResultIdx);
      if (i === activeResultIdx) el.scrollIntoView({ block: "nearest" });
    });
  }

  function renderResults(query) {
    if (!results) return;
    activeResultIdx = -1;
    const normalized = query.trim().toLowerCase();
    if (!normalized) {
      results.innerHTML = `<p class="search-hint">${escapeHtml(uiString("hint", "Type to search the documentation."))}</p>`;
      return;
    }
    const matches = index
      .filter((item) => {
        const haystack = [item.title, item.description, ...(item.headings || []), ...(item.aliases || [])]
          .join(" ")
          .toLowerCase();
        return normalized.split(/\s+/).every((part) => haystack.includes(part));
      })
      .slice(0, 10);
    if (!matches.length) {
      results.innerHTML = `<p class="search-hint">${escapeHtml(uiString("noResults", "No results for %s.").replace("%s", query))}</p>`;
      return;
    }
    activeResultIdx = 0;
    results.innerHTML = `<ul>${matches
      .map(
        (item, i) =>
          `<li><a href="${escapeHtml(item.href)}" class="${i === 0 ? "is-selected" : ""}"><strong>${escapeHtml(item.title)}</strong><span>${escapeHtml(item.description || "")}</span></a></li>`
      )
      .join("")}</ul>`;
  }

  document.querySelectorAll("[data-search-open]").forEach((button) => button.addEventListener("click", openSearch));
  document.querySelector("[data-search-close]")?.addEventListener("click", closeSearch);
  searchInput?.addEventListener("input", (event) => renderResults(event.target.value));
  searchInput?.addEventListener("keydown", (event) => {
    if (!results) return;
    const links = [...results.querySelectorAll("li a")];
    if (!links.length) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      activeResultIdx = (activeResultIdx + 1) % links.length;
      syncActiveResult();
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      activeResultIdx = (activeResultIdx - 1 + links.length) % links.length;
      syncActiveResult();
    } else if (event.key === "Enter" && activeResultIdx >= 0 && links[activeResultIdx]) {
      event.preventDefault();
      window.location.href = links[activeResultIdx].getAttribute("href") || "#";
    }
  });
  dialog?.addEventListener("click", (event) => {
    if (event.target === dialog) closeSearch();
  });

  document.addEventListener("keydown", (event) => {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      openSearch();
      return;
    }
    if (event.key === "Escape") {
      setDrawer(false);
      closeSearch();
    }
  });

  // Active-section scrollspy on right-hand TOC.
  const tocLinks = [...document.querySelectorAll(".toc ol a[href^='#']")];
  if (tocLinks.length > 0) {
    const pairs = tocLinks
      .map((link) => {
        const id = decodeURIComponent((link.getAttribute("href") || "").slice(1));
        const target = id ? document.getElementById(id) : null;
        return target ? { link, target } : null;
      })
      .filter(Boolean);

    function updateActiveToc() {
      if (!pairs.length) return;
      const offset = 110;
      let active = pairs[0];
      for (const item of pairs) {
        if (item.target.getBoundingClientRect().top <= offset) {
          active = item;
        } else {
          break;
        }
      }
      for (const item of pairs) {
        item.link.classList.toggle("is-active", item === active);
      }
    }

    window.addEventListener("scroll", updateActiveToc, { passive: true });
    updateActiveToc();
  }

  window.matchMedia("(prefers-color-scheme: dark)").addEventListener?.("change", (event) => {
    try {
      if (!localStorage.getItem(themeKey)) applyTheme(event.matches ? "dark" : "light");
    } catch {
      applyTheme(event.matches ? "dark" : "light");
    }
  });
})();
