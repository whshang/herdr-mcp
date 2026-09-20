# Extension privacy

*Browser-extension data handling, permissions, and privacy policy.*

**Effective date:** 2026-08-28

This policy describes how the Herdr browser extension handles user data. It applies to the Chrome extension distributed by the Herdr project and should be read together with the extension's [product documentation](extension.md).

The extension's single purpose is to connect supported Web AI conversations to the user's local Herdr / herdr-mcp workstation so the browser can show live workspace state, bind conversations to workspaces, preserve long-task and long-conversation continuity, queue the user's next turn, and provide bounded recovery and control UI in the Chrome Side Panel.

## Data the extension handles

To provide that user-facing functionality, the extension may handle the following data on supported Web AI sites:

- **Website content and personal communications:** conversation text and page state needed for continuity, queued messages, handoff/recovery, optional LLM analysis, and the user-invoked handoff fallback described below.
- **Web history:** the current supported-site URL, conversation/project identifiers derived from that URL, and limited navigation state needed to associate the active page with a Herdr workspace. The extension does not build or sell a general-purpose browsing-history profile.
- **User activity:** turn state, submit/settle/recovery timestamps, extension button/toggle state, and other bounded interaction state needed to determine when continuity and recovery actions are safe.
- **Authentication information:** current semantic-provider API keys are not stored or used by the extension. During upgrade, the extension may read historical browser-stored provider settings only to remove those retired credentials and permissions.
- **Local Herdr state:** workspace, pane, agent, status, output-tail, binding, and pinned-target information returned by the locally installed Herdr / herdr-mcp runtime.

The extension does not request or intentionally collect health information, financial/payment information, precise location, or data for advertising profiles.

## Where data is stored

The extension uses `chrome.storage.local` to keep settings and continuity state on the user's Chrome profile, including:

- workspace/conversation bindings;
- queued next-turn messages;
- automation preferences and recovery budgets/state;
- pinned local targets and locale settings;
- the local herdr-mcp endpoint configuration.

This local state exists so the Manifest V3 service worker and browser pages can recover safely after Chrome suspends or reloads them. The publisher does not operate an extension analytics or telemetry service that receives this local state.

Users can remove this locally stored extension data by removing the extension or clearing its extension/site data in Chrome. Semantic-provider routes and credentials stay outside extension storage: workstation-specific values live only in mode-`0600` herdr-mcp `config.toml`, while fleet-wide fallback values live in the authenticated Cloudflare Worker route pool.

## Network destinations

The extension communicates only as needed for its user-facing features:

1. **Local Herdr / herdr-mcp on the same computer.** Native Messaging is used to exchange bounded requests and live workspace state with the installed native host. This traffic stays on the user's computer.
2. **Supported and experimental Web AI sites.** The extension runs on documented browser surfaces to observe the current conversation state and perform user-facing continuity/recovery interactions. ChatGPT is the primary supported surface and Claude uses its documented adapter. z.ai and DeepSeek are experimental integrations, are disabled by default, and their content scripts are registered only after the user explicitly enables the corresponding switch in Herdr Settings and grants Chrome access to that exact site.
3. **User-configured semantic routes through herdr-mcp.** The extension sends bounded semantic inputs only to the local herdr-mcp Runtime. Runtime may route typed evaluation to user-configured TypeSafe System One, OpenRouter Decisions, or Vercel Evaluation endpoints and may route bounded Goal-Supervisor or handoff-fallback text to a user-configured OpenAI-compatible chat endpoint. Ordinary Auto can include a bounded latest user/assistant turn; Goal-aware Auto can additionally include a bounded objective/open-TODO/runtime summary plus recent user/assistant text; handoff fallback can include a bounded source transcript (currently at most 70,000 characters, preserving early task framing plus recent operational state when truncation is required). Planning and Work Memory may send their own bounded relevance inputs through the same Runtime route pool. Routes can be configured locally or on the enrolled Cloudflare Worker. When a Worker route is used, provider credentials remain at Edge and are never returned to the extension or workstation; only bounded semantic/chat results return. Each provider endpoint is user-selected and its own privacy and retention terms apply.

The extension does not sell user data, send user data to advertising networks, or transfer user data for unrelated profiling or credit/lending decisions.

## Permissions and remote code

The extension requests Chrome permissions only to provide the described functionality:

- `storage` — persist local settings and continuity state;
- `scripting` — recover/reinject the packaged content-script stack on supported Web AI tabs after MV3/page reloads and perform bounded browser-side continuity actions;
- `alarms` — wake the MV3 service worker periodically so it can restore missing local Herdr state streams and timers after Chrome suspends it;
- `nativeMessaging` — connect to the locally installed herdr-mcp native host;
- `sidePanel` — host Herdr Browser Control Center;
- host access — always-on access is limited to the documented ChatGPT/Claude surfaces and the local herdr-mcp endpoint. Experimental z.ai/DeepSeek access uses optional Chrome host permissions requested only after the user explicitly enables the corresponding integration. Semantic providers are contacted by Runtime/Edge rather than directly by the extension, so provider routes do not require Chrome host permissions. Herdr does not require `<all_urls>` as an always-on host permission.

**No remote executable code is used.** All executable JavaScript is packaged with the extension. Network responses are handled as data and are not evaluated, imported, or executed as JavaScript or Wasm.

## Limited Use

Use of information received through Chrome APIs complies with the Chrome Web Store User Data Policy, including its Limited Use requirements. In particular:

- user data is used only to provide or improve the extension's single purpose and user-facing features;
- user data is not sold or transferred to third parties outside permitted/necessary uses for those user-facing features;
- user data is not used for personalized or interest-based advertising;
- user data is not used to determine creditworthiness or for lending purposes;
- the publisher does not permit humans to read users' extension data except when the user explicitly asks for support involving specific data, or when required for security or legal compliance.

Chrome Web Store policy reference: <https://developer.chrome.com/docs/webstore/user_data>

## Security

Public network connections initiated by the extension use HTTPS/WSS where applicable. Native Messaging traffic between the extension and the native program on the same computer remains local. Semantic-provider secrets live only in mode-`0600` workstation `config.toml` or the authenticated Cloudflare Worker-wide route pool; the extension does not retain them as active configuration, and they are not intentionally written to project repositories or publisher telemetry.

## Changes to this policy

If extension behavior changes in a way that materially changes data handling, this policy and the Chrome Web Store disclosures will be updated before that behavior is published.

## Contact and support

Project homepage: <https://whshang.github.io/herdr-mcp/>

Support and issue tracker: <https://github.com/whshang/herdr-mcp/issues>
