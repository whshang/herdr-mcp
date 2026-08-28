# Browser extension privacy policy

**Effective date:** 2026-08-28

This policy describes how the Herdr browser extension handles user data. It applies to the Chrome extension distributed by the Herdr project and should be read together with the extension's [product documentation](extension.md).

The extension's single purpose is to connect supported Web AI conversations to the user's local Herdr / herdr-mcp workstation so the browser can show live workspace state, bind conversations to workspaces, preserve long-task and long-conversation continuity, queue the user's next turn, and provide bounded recovery and control UI in the Chrome Side Panel.

## Data the extension handles

To provide that user-facing functionality, the extension may handle the following data on supported Web AI sites:

- **Website content and personal communications:** conversation text and page state needed for continuity, queued messages, handoff/recovery, optional LLM analysis, and the user-invoked handoff fallback described below.
- **Web history:** the current supported-site URL, conversation/project identifiers derived from that URL, and limited navigation state needed to associate the active page with a Herdr workspace. The extension does not build or sell a general-purpose browsing-history profile.
- **User activity:** turn state, submit/settle/recovery timestamps, extension button/toggle state, and other bounded interaction state needed to determine when continuity and recovery actions are safe.
- **Authentication information:** an optional API key for a user-configured OpenAI-compatible LLM endpoint when the user explicitly configures that feature.
- **Local Herdr state:** workspace, pane, agent, status, output-tail, binding, and pinned-target information returned by the locally installed Herdr / herdr-mcp runtime.

The extension does not request or intentionally collect health information, financial/payment information, precise location, or data for advertising profiles.

## Where data is stored

The extension uses `chrome.storage.local` to keep settings and continuity state on the user's Chrome profile, including:

- workspace/conversation bindings;
- queued next-turn messages;
- automation preferences and recovery budgets/state;
- pinned local targets and locale settings;
- the local herdr-mcp endpoint configuration;
- optional user-configured LLM endpoint, model, and API key.

This local state exists so the Manifest V3 service worker and browser pages can recover safely after Chrome suspends or reloads them. The publisher does not operate an extension analytics or telemetry service that receives this local state.

Users can remove this locally stored extension data by removing the extension or clearing its extension/site data in Chrome. Optional LLM configuration can also be removed from the extension settings.

## Network destinations

The extension communicates only as needed for its user-facing features:

1. **Local Herdr / herdr-mcp on the same computer.** Native Messaging is used to exchange bounded requests and live workspace state with the installed native host. This traffic stays on the user's computer.
2. **Supported and experimental Web AI sites.** The extension runs on documented browser surfaces to observe the current conversation state and perform user-facing continuity/recovery interactions. ChatGPT is the primary supported surface and Claude uses its documented adapter. z.ai and DeepSeek are experimental integrations, are disabled by default, and their content scripts are registered only after the user explicitly enables the corresponding switch in Herdr Settings.
3. **A user-configured LLM endpoint, only for configured LLM features.** If the user configures an OpenAI-compatible LLM endpoint, the extension can send relevant user/assistant text plus the user-supplied API credential to that endpoint for optional post-turn analysis. If conversation handoff is invoked by the user, or is triggered by an Auto policy the user enabled, and the current Web AI conversation cannot produce the required handoff summary because it has reached a hard conversation limit, the handoff prompt cannot be submitted, or the primary summary settles without a valid packet, the extension can instead send a bounded source transcript to that same configured endpoint to generate the handoff packet. The fallback transcript contains only user/assistant conversation text selected by the extension and is bounded to the extension's handoff limit (currently 70,000 characters, preserving early task framing plus recent operational state when truncation is required). The endpoint is chosen by the user and is not selected or operated by the Herdr publisher by default; the endpoint provider's own privacy and retention terms apply.

The extension does not sell user data, send user data to advertising networks, or transfer user data for unrelated profiling or credit/lending decisions.

## Permissions and remote code

The extension requests Chrome permissions only to provide the described functionality:

- `storage` — persist local settings and continuity state;
- `scripting` — recover/reinject the packaged content-script stack on supported Web AI tabs after MV3/page reloads and perform bounded browser-side continuity actions;
- `alarms` — wake the MV3 service worker periodically so it can restore missing local Herdr state streams and timers after Chrome suspends it;
- `nativeMessaging` — connect to the locally installed herdr-mcp native host;
- `sidePanel` — host Herdr Browser Control Center;
- host access — operate on supported Web AI sites, the local herdr-mcp endpoint, and an optional endpoint explicitly configured by the user for LLM analysis and handoff-summary fallback.

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

Public network connections initiated by the extension use HTTPS/WSS where applicable. Native Messaging traffic between the extension and the native program on the same computer remains local. Secrets such as an optional LLM API key are not intentionally written to project repositories or publisher telemetry.

## Changes to this policy

If extension behavior changes in a way that materially changes data handling, this policy and the Chrome Web Store disclosures will be updated before that behavior is published.

## Contact and support

Project homepage: <https://whshang.github.io/herdr-mcp/>

Support and issue tracker: <https://github.com/whshang/herdr-mcp/issues>
