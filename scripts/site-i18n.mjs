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
  "overview",
  "design-philosophy",
  "quick-start",
  "install",
  "clean-machine-uat",
  "chatgpt-connector",
  "browser-continuity",
  "extension",
  "browser-control-center",
  "extension-wake",
  "extension-bridge",
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
  "troubleshooting",
];

// Group membership is keyed by logical slug only; labels are translated in UI.
export const NAV_GROUPS = [
  { slugs: ["overview", "design-philosophy", "quick-start", "install", "clean-machine-uat", "chatgpt-connector"] },
  { slugs: ["browser-continuity", "extension", "browser-control-center", "extension-wake", "extension-bridge"] },
  { slugs: ["architecture", "best-practices", "cli-reference"] },
  { slugs: ["cloudflare-edge-deployment", "cloudflare-edge-token", "runtime-self-upgrade"] },
  { slugs: ["agent-install", "automation", "capability-benchmark", "herdr-vs-ecosystem", "worker-fallbacks"] },
  { slugs: ["troubleshooting"] },
];

// Per-locale navigation group labels, in NAV_GROUPS order.
export const NAV_GROUP_LABELS = {
  "zh-CN": ["开始", "浏览器", "工作方式与架构", "部署与运行", "高级参考", "排障"],
  en: ["Start", "Browser", "Workflow & architecture", "Deploy & operate", "Advanced reference", "Troubleshooting"],
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
      "herdr-mcp 把 ChatGPT 等 Web AI 连接到本地 Herdr 工作站：直接读改代码、运行 Git/Shell、调度本地 Agent，并通过浏览器连续工作跨越长任务和长对话。第一次使用从总览与快速开始进入。",
    indexCtaConnect: "快速开始",
    indexCtaArchitecture: "总览",
    indexCtaDeploy: "连接 ChatGPT",
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
    indexTitle: "Connect Web AI to the real development machine.",
    indexLead:
      "herdr-mcp connects ChatGPT and other Web AI to a local Herdr workstation for direct code, Git and shell work, local-agent delegation, and browser continuity across long tasks and long conversations. Start with Overview and Quick start.",
    indexCtaConnect: "Quick start",
    indexCtaArchitecture: "Overview",
    indexCtaDeploy: "Connect ChatGPT",
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
