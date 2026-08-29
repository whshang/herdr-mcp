// Single source of truth for the documentation site's logical-document model.
//
// The ordered list of logical documents and their group membership are defined
// exactly once here, keyed by logical slug. Each locale directory under
// docs/i18n/<locale>/ supplies a translated variant of every slug; navigation
// group labels and user-visible UI strings are the only locale-specific
// presentation data and live in UI below.

export const SITE_ORIGIN_ENV = "HERDR_SITE_ORIGIN";
export const DEFAULT_ORIGIN = "https://whshang.github.io/herdr-mcp";

// English is the canonical/default locale. The neutral /docs/ router still
// honors an unpinned zh browser preference before landing on a locale homepage.
export const DEFAULT_LOCALE = "en";
export const LOCALES = ["en", "zh-CN"];
export const LOCALE_NAMES = { en: "English", "zh-CN": "简体中文" };

// Logical document catalog order — defines sidebar/search/build order in every
// locale. Maintainer-only references remain discoverable but are deliberately
// excluded from the ordinary next/previous reading flow below.
export const DOC_ORDER = [
  "quick-agent-install",
  "overview",
  "install",
  "chatgpt-connector",
  "quick-start",
  "best-practices",
  "automation",
  "cli-reference",
  "browser-continuity",
  "extension",
  "browser-control-center",
  "privacy",
  "cloudflare-edge-deployment",
  "cloudflare-edge-token",
  "runtime-self-upgrade",
  "worker-fallbacks",
  "troubleshooting",
  "design-philosophy",
  "architecture",
  "capability-benchmark",
  "herdr-vs-ecosystem",
  "remote-coding-ecosystem-research",
  "extension-wake",
  "extension-bridge",
  "agent-install",
  "clean-machine-uat",
];

export const MAINTAINER_DOCS = [
  "extension-wake",
  "extension-bridge",
  "agent-install",
  "clean-machine-uat",
];

export const READING_ORDER = DOC_ORDER.filter((slug) => !MAINTAINER_DOCS.includes(slug));

// Group membership is keyed by logical slug only; labels are translated in UI.
export const NAV_GROUPS = [
  { slugs: ["quick-agent-install", "overview", "install", "chatgpt-connector", "quick-start"] },
  { slugs: ["best-practices", "automation", "cli-reference"] },
  { slugs: ["browser-continuity", "extension", "browser-control-center", "privacy"] },
  { slugs: ["cloudflare-edge-deployment", "cloudflare-edge-token", "runtime-self-upgrade", "worker-fallbacks", "troubleshooting"] },
  { slugs: ["design-philosophy", "architecture", "capability-benchmark", "herdr-vs-ecosystem", "remote-coding-ecosystem-research"] },
  { slugs: MAINTAINER_DOCS, secondary: true },
];

// Per-locale navigation group labels, in NAV_GROUPS order.
export const NAV_GROUP_LABELS = {
  "zh-CN": ["开始", "使用 herdr-mcp", "浏览器（可选）", "运维与排障", "架构与进阶", "维护者参考"],
  en: ["Start", "Use herdr-mcp", "Browser (optional)", "Operate & troubleshoot", "Architecture & advanced", "Maintainer reference"],
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
    versionBadgeAria: "源码版本",
    agentIntroTitle: "最快方式：让 Coding Agent 直接安装",
    agentIntroLead: "把下面提示词交给本地 Coding Agent。它应该先读完整安装协议，再自动执行能够自动化的步骤。",
    agentPrompt:
      "请帮我安装并配置 Herdr 和 herdr-mcp，请先完整阅读并严格按照这个指引执行：https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/zh-CN/quick-agent-install.md 。herdr-mcp 本机 runtime 使用 GitHub Releases，不用 git clone。只在 Cloudflare 登录/创建 API Token，以及 ChatGPT 添加 herdr Connector/插件这两类需要我本人操作的步骤暂停并指导我，其余步骤请自动完成并验证。",
    agentSkillLink: "阅读完整快速 Agent 安装协议",
    homeWillDoTitle: "Coding Agent 会做什么",
    homeWillDoLead: "它负责自动化安装、部署和验证；只有必须由你本人完成的身份授权才停下来。",
    homeWillDoHerdr: "检查或安装 Herdr，并从 GitHub Releases 安装稳定 herdr-mcp。",
    homeWillDoEdge: "部署并配置 Edge + Link，把 Web AI 安全连接到本机 Herdr。",
    homeWillDoVerify: "运行 runtime / doctor 验证，发现问题先修复再继续。",
    homeHandoffsTitle: "你只需要接管两次",
    homeCloudflareTitle: "1. Cloudflare 登录 / Token",
    homeCloudflareBody: "Agent 会把你带到正确页面；登录或创建所需 Token 后，把控制权交还给 Agent。",
    homeChatgptTitle: "2. ChatGPT Connector",
    homeChatgptBody: "在 ChatGPT 添加 herdr Connector 并完成 OAuth；其余配置和验证继续由 Agent 完成。",
    homePathsTitle: "选择你的路径",
    homeAgentPathTitle: "Agent 辅助安装（推荐）",
    homeAgentPathBody: "复制上面的提示词，让 Coding Agent 按完整协议执行。",
    homeManualPathTitle: "手动安装",
    homeManualPathBody: "需要自己控制每个步骤时，使用人工安装参考。",
    homeBrowserPathTitle: "浏览器扩展（可选）",
    homeBrowserPathBody: "从 Chrome Web Store 安装，用于对话接力、状态 HUD 和 Browser Control Center。",
    homeOutcomesTitle: "安装后你会得到",
    homeOutcome1: "从远程 MCP 安全访问本机 Herdr workspaces / Agents。",
    homeOutcome2: "稳定的 ChatGPT Connector 路径与可验证的本地 Agent 能力。",
    homeOutcome3: "可选的浏览器连续性与控制界面，以及安全更新 / rollback。",
    homeSafetyTitle: "安全边界",
    homeSafetyBody: "浏览器页面不持有长期本地 bearer；Native Messaging 保持本机、精确扩展身份；runtime 与 extension 独立更新；mutation 仍保持目标与交付状态可判定。",
    homeSupportTitle: "需要更多信息？",
    homeHistory: "历史与发布证据",
    historyNav: "History / 历史证据",
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
    versionBadgeAria: "Source version",
    agentIntroTitle: "Fastest path: let your coding agent install it",
    agentIntroLead:
      "Give this prompt to your local coding agent. It should read the complete install protocol first, then automate everything that does not require you personally.",
    agentPrompt:
      "Install and configure Herdr and herdr-mcp for me. First read and follow this guide end to end: https://raw.githubusercontent.com/whshang/herdr-mcp/main/docs/i18n/en/quick-agent-install.md . Install the local herdr-mcp runtime from GitHub Releases, not from a git clone. Pause only when I personally need to sign in/create a Cloudflare API Token, or when I need to add the herdr Connector/app in ChatGPT. Automate and verify everything else.",
    agentSkillLink: "Read the complete quick agent install protocol",
    homeWillDoTitle: "What the coding agent will do",
    homeWillDoLead: "It automates install, deployment, and verification, pausing only for identity steps that require you personally.",
    homeWillDoHerdr: "Check or install Herdr, then install stable herdr-mcp from GitHub Releases.",
    homeWillDoEdge: "Deploy and configure Edge + Link so Web AI can safely reach your local Herdr.",
    homeWillDoVerify: "Run runtime / doctor verification and fix failures before continuing.",
    homeHandoffsTitle: "You take over only twice",
    homeCloudflareTitle: "1. Cloudflare sign-in / token",
    homeCloudflareBody: "The agent takes you to the right page; sign in or create the required token, then hand control back.",
    homeChatgptTitle: "2. ChatGPT Connector",
    homeChatgptBody: "Add the herdr Connector in ChatGPT and complete OAuth; the agent continues configuration and verification.",
    homePathsTitle: "Choose your path",
    homeAgentPathTitle: "Agent-assisted install (recommended)",
    homeAgentPathBody: "Copy the prompt above and let your coding agent follow the full protocol.",
    homeManualPathTitle: "Manual install",
    homeManualPathBody: "Use the human install reference when you want to control every step yourself.",
    homeBrowserPathTitle: "Browser extension (optional)",
    homeBrowserPathBody: "Install from the Chrome Web Store for conversation continuity, the status HUD, and Browser Control Center.",
    homeOutcomesTitle: "What you get",
    homeOutcome1: "Safe remote MCP access to local Herdr workspaces and Agents.",
    homeOutcome2: "A stable ChatGPT Connector path with evidence-backed local Agent capability awareness.",
    homeOutcome3: "Optional browser continuity/control surfaces plus safe update and rollback.",
    homeSafetyTitle: "Safety boundary",
    homeSafetyBody: "Browser pages never hold the long-lived local bearer; Native Messaging stays local with an exact extension identity; runtime and extension update independently; mutations remain target- and delivery-aware.",
    homeSupportTitle: "Need more detail?",
    homeHistory: "History and release evidence",
    historyNav: "History / evidence",
    copyCode: "Copy",
    copiedCode: "Copied",
  },
};
