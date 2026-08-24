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
  "architecture",
  "install",
  "agent-install",
  "chatgpt-connector",
  "worker-fallbacks",
  "cloudflare-edge-deployment",
  "cloudflare-edge-token",
  "runtime-self-upgrade",
  "automation",
  "extension",
  "extension-wake",
  "extension-bridge",
  "capability-benchmark",
  "cli-reference",
  "best-practices",
  "troubleshooting",
];

// Group membership is keyed by logical slug only; labels are translated in UI.
export const NAV_GROUPS = [
  { slugs: ["architecture", "install", "agent-install", "chatgpt-connector"] },
  { slugs: ["worker-fallbacks"] },
  {
    slugs: ["cloudflare-edge-deployment", "cloudflare-edge-token", "runtime-self-upgrade", "automation"],
  },
  { slugs: ["extension", "extension-wake", "extension-bridge"] },
  { slugs: ["capability-benchmark", "cli-reference", "best-practices"] },
  { slugs: ["troubleshooting"] },
];

// Per-locale navigation group labels, in NAV_GROUPS order.
export const NAV_GROUP_LABELS = {
  "zh-CN": ["从这里开始", "ChatGPT 与 Web", "运维与部署", "浏览器扩展", "参考与开发", "帮助"],
  en: ["Start here", "ChatGPT & Web", "Operations & Deployment", "Browser Extension", "Reference & Development", "Help"],
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
    indexTitle: "远程规划，本机执行。",
    indexLead:
      "herdr-mcp 是 ChatGPT 或其它网页模型与本地 Herdr 工作站之间的远程控制面。建议从系统边界开始，连接 ChatGPT，再按需选择部署与浏览器工作流。",
    indexCtaConnect: "连接 ChatGPT",
    indexCtaArchitecture: "架构",
    indexCtaDeploy: "部署 Edge",
    indexHome: "首页",
    indexSource: "源码",
    indexFooterAria: "文档索引底部导航",
    versionBadgeAria: "当前版本",
    agentIntroTitle: "或者，让 agent 带你上手",
    agentIntroLead: "如果你已经有一个 AI 编码 agent，让它来引导你：把这句提示语粘贴给 agent。",
    agentPrompt:
      "请先阅读 https://whshang.github.io/herdr-mcp/herdr-mcp-SKILL.md，然后一步步带我理解并配置 herdr-mcp。",
    agentSkillLink: "阅读 herdr-mcp-SKILL.md（agent 版项目手册）",
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
    indexTitle: "Remote planning, local execution.",
    indexLead:
      "herdr-mcp is the remote control plane between ChatGPT or another web planner and a local Herdr workstation. Start with the system boundary, connect ChatGPT, then choose the deployment and browser workflows you need.",
    indexCtaConnect: "Connect ChatGPT",
    indexCtaArchitecture: "Architecture",
    indexCtaDeploy: "Deploy the Edge",
    indexHome: "Home",
    indexSource: "Source",
    indexFooterAria: "Documentation index footer",
    versionBadgeAria: "Current version",
    agentIntroTitle: "Or let your agent introduce you",
    agentIntroLead:
      "If you already run an AI coding agent, let it handle the onboarding. Paste this prompt to your agent:",
    agentPrompt:
      "Help me understand and set up herdr-mcp. Read https://whshang.github.io/herdr-mcp/herdr-mcp-SKILL.md first, then walk me through it step by step.",
    agentSkillLink: "Read herdr-mcp-SKILL.md (the agent-facing project guide)",
    copyCode: "Copy",
    copiedCode: "Copied",
  },
};
