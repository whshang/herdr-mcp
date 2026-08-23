(() => {
  const root = document.documentElement;
  const body = document.body;
  const themeButton = document.querySelector("[data-theme-toggle]");
  const themeIcon = document.querySelector("[data-theme-icon]");
  const themeKey = "herdr-docs-theme";

  const systemTheme = () => window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  const savedTheme = localStorage.getItem(themeKey);
  const initialTheme = savedTheme === "light" || savedTheme === "dark" ? savedTheme : systemTheme();

  function applyTheme(theme) {
    root.dataset.theme = theme;
    root.style.colorScheme = theme;
    if (themeButton) themeButton.setAttribute("aria-label", `Switch to ${theme === "dark" ? "light" : "dark"} theme`);
    if (themeIcon) themeIcon.textContent = theme === "dark" ? "☀" : "◐";
  }

  applyTheme(initialTheme);
  themeButton?.addEventListener("click", () => {
    const next = root.dataset.theme === "dark" ? "light" : "dark";
    localStorage.setItem(themeKey, next);
    applyTheme(next);
  });

  const navToggle = document.querySelector("[data-nav-toggle]");
  const sidebar = document.querySelector("[data-sidebar]");
  const overlay = document.querySelector("[data-nav-overlay]");

  function setDrawer(open) {
    if (!sidebar || !navToggle) return;
    body.classList.toggle("nav-open", open);
    navToggle.setAttribute("aria-expanded", String(open));
    navToggle.setAttribute("aria-label", open ? "Close documentation navigation" : "Open documentation navigation");
    if (overlay) overlay.hidden = !open;
  }

  navToggle?.addEventListener("click", () => setDrawer(!body.classList.contains("nav-open")));
  overlay?.addEventListener("click", () => setDrawer(false));
  sidebar?.addEventListener("click", (event) => {
    if (event.target instanceof HTMLAnchorElement && window.matchMedia("(max-width: 1023px)").matches) setDrawer(false);
  });

  const dialog = document.querySelector("[data-search-dialog]");
  const searchInput = document.querySelector("[data-search-input]");
  const results = document.querySelector("[data-search-results]");
  const searchData = document.querySelector("#search-index");
  let index = [];
  if (searchData) {
    try { index = JSON.parse(searchData.textContent || "[]"); } catch { index = []; }
  }

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

  function renderResults(query) {
    if (!results) return;
    const normalized = query.trim().toLowerCase();
    if (!normalized) {
      results.innerHTML = '<p class="search-hint">Type to search the documentation.</p>';
      return;
    }
    const matches = index.filter((item) => {
      const haystack = [item.title, item.description, ...(item.headings || [])].join(" ").toLowerCase();
      return normalized.split(/\s+/).every((part) => haystack.includes(part));
    }).slice(0, 8);
    if (!matches.length) {
      results.innerHTML = `<p class="search-hint">No results for <strong>${escapeHtml(query)}</strong>.</p>`;
      return;
    }
    results.innerHTML = `<ul>${matches.map((item) => `<li><a href="${escapeHtml(item.href)}"><strong>${escapeHtml(item.title)}</strong><span>${escapeHtml(item.description || "")}</span></a></li>`).join("")}</ul>`;
  }

  document.querySelectorAll("[data-search-open]").forEach((button) => button.addEventListener("click", openSearch));
  document.querySelector("[data-search-close]")?.addEventListener("click", closeSearch);
  searchInput?.addEventListener("input", (event) => renderResults(event.target.value));
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

  window.matchMedia("(prefers-color-scheme: dark)").addEventListener?.("change", (event) => {
    if (!localStorage.getItem(themeKey)) applyTheme(event.matches ? "dark" : "light");
  });
})();
