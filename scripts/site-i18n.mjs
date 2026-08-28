// Single source of truth for the documentation site's logical-document model.
//
// The ordered list of logical documents and their group membership are defined
// exactly once here, keyed by logical slug. Each locale directory under
// docs/i18n/<locale>/ supplies a translated variant of every slug; navigation
// group labels and user-visible UI strings are the only locale-specific
// presentation data and live in UI below.

export const SITE_ORIGIN_ENV = "HERDR_SITE_ORIGIN";
export const DEFAULT_ORIGIN = "https://whshang.github.io/herdr-mcp";

// English is the default locale: /docs/ lands on it directly, and the topbar
// language switcher is how readers reach the Chinese docs.
export const DEFAULT_LOCALE = "en";
export const LOCALES = ["en", "zh-CN"];
export const LOCALE_NAMES = { en: "English", "zh-CN": "简体中文" };

// Logical document order — defines prev/next order in every locale.
export const DOC_ORDER = [
  "quick-agent-install",
  "overview",
  "install",
  "chatgpt-connector",
  "quick-start",
  "browser-continuity",
  "extension",
  "privacy",
  "browser-control-center",
  "extension-wake",
  "extension-bridge",
  "design-philosophy",
  "architecture",
  "best-practices",
  "cli-reference",
  "cloudflare-edge-deployment",
  "cloudflare-edge-token",
  "runtime-self-upgrade",
  "agent-install",
  "automation",
  "capability-benchmark",
  "herdr-vs-ecosystem",
  "worker-fallbacks",
  "clean-machine-uat",
  "troubleshooting",
];

// Group membership is keyed by logical slug only; labels are translated in UI.
export const NAV_GROUPS = [
  { slugs: ["quick-agent-install", "overview", "install", "chatgpt-connector", "quick-start"] },
  { slugs: ["browser-continuity", "extension", "privacy", "browser-control-center", "extension-wake", "extension-bridge"] },
  { slugs: ["design-philosophy", "architecture", "best-practices", "cli-reference"] },
  { slugs: ["cloudflare-edge-deployment", "cloudflare-edge-token", "runtime-self-upgrade"] },
  { slugs: ["agent-install", "automation", "capability-benchmark", "herdr-vs-ecosystem", "worker-fallbacks", "clean-machine-uat"] },
  { slugs: ["troubleshooting"] },
];

// Per-locale navigation group labels, in NAV_GROUPS order.
export const NAV_GROUP_LABELS = {
  "zh-CN": ["开始使用", "浏览器（可选）", "理解与日常使用", "部署与运行", "维护者参考", "排障"],
  en: ["Get started", "Browser (optional)", "Understand & use", "Deploy & operate", "Maintainer reference", "Troubleshooting"],
};

// Per-locale user-visible strings. The build fails fast if a label is missing,
// so both locales must stay complete.
export const UI = {
  "zh-CN": {
    htmlLang: "zh-CN",
    docsNav: "文档",
    brandHomeAria: "herdr-mcp 首页",
    langSwitcherAria: "切换语言",
    searchTriggerAria: "打开搜索",
    searchLabel: "搜索",
    searchCloseAria: "关闭搜索",
    searchEyebrow: "文档",
    searchTitle: "搜索 herdr-mcp",
    searchPlaceholder: "搜索页面和章节…",
    searchHint: "输入以搜索文档。",
    searchNoResults: "未找到“%s”的相关结果。",
    openNav: "打开文档导航",
    closeNav: "关闭文档导航",
    themeToggleAria: "切换颜色主题",
    themeToLight: "切换到浅色主题",
    themeToDark: "切换到深色主题",
    docsSidebarAria: "文档导航",
    onThisPage: "本页目录",
    tocEmpty: "无章节",
    previous: "上一篇",
    next: "下一篇",
    pageNavAria: "相邻文档页面",
    docsIndex: "文档索引",
    editSource: "编辑源文件",
    indexEyebrow: "文档",
    indexTitle: "让 Web AI 真正接上你的开发机。",
    indexLead:
      "第一次使用不需要先学架构。把一条安装提示词交给 Cursor、Codex、Claude Code 等本地 Coding Agent；它会安装 Herdr 和 herdr-mcp，只在 Cloudflare 和 ChatGPT 必须人工操作时暂停。",
    indexCtaConnect: "让 Agent 帮我安装",
    indexCtaArchitecture: "总览",
    indexCtaDeploy: "连接 ChatGPT",
    indexHome: "首页",
    indexSource: "源码",
    indexFooterAria: "文档索引底部导航",
    versionBadgeAria: "当前版本",
    agentIntroTitle: "最快方式：让 Coding Agent 直接安装",
    agentIntroLead: "把下面提示词交给本地 Coding Agent。它应该先读完整安装协议，再自动执行能够自动化的步骤。",
    agentPrompt:
      "请帮我安装并配置 Herdr 和 herdr-mcp，请先完整阅读并严格按照这个指引执行：https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/quick-agent-install.md 。herdr-mcp 本机 runtime 使用 GitHub Releases，不用 git clone。只在 Cloudflare 登录/创建 API Token，以及 ChatGPT 添加 herdr Connector/插件这两类需要我本人操作的步骤暂停并指导我，其余步骤请自动完成并验证。",
    agentSkillLink: "阅读完整快速 Agent 安装协议",
    copyCode: "复制",
    copiedCode: "已复制",
  },
  en: {
    htmlLang: "en",
    docsNav: "Docs",
    brandHomeAria: "herdr-mcp home",
    langSwitcherAria: "Change language",
    searchTriggerAria: "Open search",
    searchLabel: "Search",
    searchCloseAria: "Close search",
    searchEyebrow: "Documentation",
    searchTitle: "Search herdr-mcp",
    searchPlaceholder: "Search pages and sections…",
    searchHint: "Type to search the documentation.",
    searchNoResults: "No results for “%s”.",
    openNav: "Open documentation navigation",
    closeNav: "Close documentation navigation",
    themeToggleAria: "Toggle color theme",
    themeToLight: "Switch to light theme",
    themeToDark: "Switch to dark theme",
    docsSidebarAria: "Documentation",
    onThisPage: "On this page",
    tocEmpty: "No sections",
    previous: "Previous",
    next: "Next",
    pageNavAria: "Adjacent documentation pages",
    docsIndex: "Documentation index",
    editSource: "Edit source",
    indexEyebrow: "Documentation",
    indexTitle: "Connect Web AI to the real development machine.",
    indexLead:
      "You do not need to learn the architecture first. Give one setup prompt to Cursor, Codex, Claude Code, or another local coding agent; it installs Herdr and herdr-mcp and pauses only for Cloudflare and ChatGPT steps that require you.",
    indexCtaConnect: "Let my agent install it",
    indexCtaArchitecture: "Overview",
    indexCtaDeploy: "Connect ChatGPT",
    indexHome: "Home",
    indexSource: "Source",
    indexFooterAria: "Documentation index footer",
    versionBadgeAria: "Current version",
    agentIntroTitle: "Fastest path: let your coding agent install it",
    agentIntroLead:
      "Give this prompt to your local coding agent. It should read the complete install protocol first, then automate everything that does not require you personally.",
    agentPrompt:
      "Install and configure Herdr and herdr-mcp for me. First read and follow this guide end to end: https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md . Install the local herdr-mcp runtime from GitHub Releases, not from a git clone. Pause only when I personally need to sign in/create a Cloudflare API Token, or when I need to add the herdr Connector/app in ChatGPT. Automate and verify everything else.",
    agentSkillLink: "Read the complete quick agent install protocol",
    copyCode: "Copy",
    copiedCode: "Copied",
  },
};
