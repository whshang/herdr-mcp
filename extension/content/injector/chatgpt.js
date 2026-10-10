// injector/chatgpt.js — chatgpt.com wake-up adapter
// Composer layouts: legacy #prompt-textarea and the id-less Project ProseMirror textbox.
//   - send button: button[data-testid="send-button"]
//   - insertion: MAIN-world execCommand insertText commits to the ProseMirror model
const CHATGPT_ADAPTER_CAPABILITIES = Object.freeze({
  browserActuation: true,
  stopGeneration: true,
  sessionCreate: true,
  sessionOpen: true,
  titleProjection: true,
  chatModeGuard: true,
});
const CHATGPT_ADAPTER_POLICY = Object.freeze({
  experimentalStorageFlag: null,
  operationalHud: true,
  submitAckTimeoutMs: 8000,
  jsonBridge: false,
});

class ChatGPTAdapter extends BaseAdapter {
  get name() { return "chatgpt"; }
  get capabilities() { return CHATGPT_ADAPTER_CAPABILITIES; }
  get policy() { return CHATGPT_ADAPTER_POLICY; }
  get needsMainWorldInsert() { return true; }

  getVisibleModeRadio(pattern) {
    return [...document.querySelectorAll('button[role="radio"]')].find((button) => {
      if (!this.elementVisible(button)) return false;
      const rect = button.getBoundingClientRect();
      const label = String(button.innerText || button.textContent || "").replace(/\s+/g, " ").trim();
      return rect.width > 0 && rect.height > 0 && pattern.test(label);
    }) || null;
  }

  async prepareBrowserActuation() {
    const chat = this.getVisibleModeRadio(/^(?:聊天|Chat|チャット)$/i);
    const work = this.getVisibleModeRadio(/^(?:工作|Work|作業)$/i);
    if (!chat && !work) return { ok: true, switched: false };
    if (!chat || !work) return { ok: false, error: "chat_mode_ambiguous" };
    if (chat.getAttribute("aria-checked") === "true") {
      return { ok: true, switched: false };
    }
    if (work.getAttribute("aria-checked") !== "true") {
      return { ok: false, error: "chat_mode_ambiguous" };
    }
    try {
      chat.click();
    } catch (_) {
      return { ok: false, error: "chat_mode_switch_failed" };
    }
    const deadline = Date.now() + 3000;
    do {
      if (chat.getAttribute("aria-checked") === "true"
          && work.getAttribute("aria-checked") === "false") {
        return { ok: true, switched: true };
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    } while (Date.now() < deadline);
    return { ok: false, error: "chat_mode_switch_timeout" };
  }

  async getAccountNativeIdentity(timeoutMs = 1500) {
    for (const url of ["/backend-api/me", "/api/auth/session"]) {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), timeoutMs);
      try {
        const response = await fetch(url, {
          method: "GET",
          credentials: "include",
          cache: "no-store",
          headers: { accept: "application/json" },
          signal: controller.signal,
        });
        if (!response.ok) continue;
        const payload = await response.json();
        const candidate = payload?.id || payload?.user?.id || payload?.account?.id || null;
        if (typeof candidate === "string" && candidate.trim()) return candidate.trim();
      } catch (_) {
      } finally {
        clearTimeout(timer);
      }
    }
    return null;
  }

  getConversationKey() {
    try {
      const origin = location.origin;
      const pathname = location.pathname.replace(/\/+$/, "") || "/";
      const provisionalChat = (segment) => /^local-chatgpt(?::|%3a)/i.test(segment);
      if (pathname === "/") return origin;
      const normal = pathname.match(/^\/c\/([^/]+)$/);
      if (normal) return provisionalChat(normal[1]) ? null : `${origin}/c/${normal[1]}`;

      const projectConversation = pathname.match(/^\/g\/(g-p-[^/]+)\/c\/([^/]+)$/i);
      const projectHome = pathname.match(/^\/g\/(g-p-[^/]+)(?:\/project)?$/i);
      if (projectConversation && provisionalChat(projectConversation[2])) return null;
      const projectSegment = projectConversation?.[1] || projectHome?.[1] || null;
      if (!projectSegment) return null;
      // ChatGPT may decorate a Project resource id with a human-readable slug.
      // Bindings intentionally normalize that cosmetic suffix away.
      const m = projectSegment.match(/^(g-p-[0-9a-f]{32})(?:-[^/]*)?$/i);
      const projectId = m ? m[1] : projectSegment;
      const projectKey = `${origin}/g/${projectId}`;
      return projectConversation ? `${projectKey}/c/${projectConversation[2]}` : projectKey;
    } catch (_) {
      return null;
    }
  }

  getInputEl() {
    const legacy = document.querySelector('#prompt-textarea[contenteditable="true"]');
    if (legacy) return legacy;
    const candidates = document.querySelectorAll('div.ProseMirror[role="textbox"][contenteditable="true"]');
    return candidates.length === 1 && this.elementVisible(candidates[0])
      ? candidates[0]
      : null;
  }

  getSelectedComposerApps() {
    const input = this.getInputEl();
    if (!input) return [];
    const selected = new Set();
    for (const pill of input.querySelectorAll('[data-inline-selection-pill][data-symbol="ecosystemMention"][data-keyword]')) {
      const keyword = String(pill.getAttribute('data-keyword') || '').trim().toLowerCase();
      if (keyword) selected.add(keyword);
    }
    for (const keyword of this.getInlineAppMentionKeywords(input)) selected.add(keyword);
    return [...selected];
  }

  // Current ChatGPT composer (live 0.1.172 evidence): a selected App is an
  // inline span with data-appearance="inline-mention" carrying provider-owned
  // app-mention-name and app-mention-path attributes. Identity is the exact
  // app-mention-name; a path is required so plain text can never qualify.
  getInlineAppMentionNodes(root) {
    if (!root) return [];
    return [...root.querySelectorAll('span[data-appearance="inline-mention"][app-mention-name][app-mention-path]')]
      .filter((node) => String(node.getAttribute('app-mention-path') || '').trim() !== '');
  }

  getInlineAppMentionKeywords(root) {
    const out = new Set();
    for (const node of this.getInlineAppMentionNodes(root)) {
      const name = String(node.getAttribute('app-mention-name') || '').trim().toLowerCase();
      if (/^[a-z0-9_-]{1,64}$/.test(name)) out.add(name);
    }
    return [...out];
  }

  getLatestUserAppKeywords(latestUser = null) {
    const latest = latestUser || (() => {
      const turns = [...document.querySelectorAll('[data-message-author-role="user"]')];
      return turns[turns.length - 1] || null;
    })();
    if (!latest) return [];
    const selected = new Set();
    for (const pill of latest.querySelectorAll('[data-inline-selection-pill][data-symbol="ecosystemMention"][data-keyword]')) {
      const keyword = String(pill.getAttribute('data-keyword') || '').trim().toLowerCase();
      if (keyword) selected.add(keyword);
    }
    for (const keyword of this.getInlineAppMentionKeywords(latest)) selected.add(keyword);
    return [...selected];
  }

  getComposerTextWithoutAppPills() {
    const input = this.getInputEl();
    if (!input) return '';
    const clone = input.cloneNode(true);
    for (const node of clone.querySelectorAll('[data-inline-selection-pill], [data-inline-selection-pill-cursor-target]')) {
      node.remove();
    }
    for (const node of this.getInlineAppMentionNodes(clone)) node.remove();
    return String(clone.textContent || '').replace(/\uFEFF/g, '').trim();
  }

  composerHasOnlyAppPills(requiredApps = []) {
    const requested = [...new Set(requiredApps.map((app) => String(app || '').trim().toLowerCase()).filter(Boolean))];
    const selected = this.getSelectedComposerApps();
    if (!requested.length || requested.some((app) => !selected.includes(app))) return false;
    return this.getComposerTextWithoutAppPills() === '';
  }

  // Live 0.1.168 evidence: after trusted `@herdr` keys ChatGPT renders the
  // mention suggestion as plain <button> rows (no menu/listbox role, outside
  // the composer form). Return visible buttons whose text is exactly the
  // keyword; callers must subtract the pre-typing baseline so only buttons
  // that appeared because of this search are candidates.
  getComposerAppSuggestionButtons(keyword) {
    const wanted = String(keyword || '').trim().toLowerCase();
    const input = this.getInputEl();
    if (!wanted || !input) return [];
    const visible = (element) => Boolean(element && (element.offsetWidth || element.offsetHeight || element.getClientRects?.().length));
    const out = new Set();
    for (const button of document.querySelectorAll('button, [role="button"]')) {
      if (!visible(button) || input.contains(button) || button.disabled) continue;
      const exactLabel = [button, ...button.querySelectorAll('span, div')].some((node) => (
        String(node.textContent || '').replace(/\s+/g, ' ').trim().toLowerCase() === wanted
      ));
      if (!exactLabel) continue;
      out.add(button);
    }
    return [...out];
  }

  getComposerAppCandidates(keyword) {
    const wanted = String(keyword || '').trim().toLowerCase();
    const input = this.getInputEl();
    if (!wanted || !input) return [];
    const visible = (element) => Boolean(element && (element.offsetWidth || element.offsetHeight || element.getClientRects?.().length));
    const menuRoots = [...document.querySelectorAll('.popover, [role="menu"], [role="listbox"]')]
      .filter((node) => visible(node) && !input.contains(node));
    if (!menuRoots.length) return [];
    const keywordMatches = (element) => {
      const nodes = [
        element,
        ...Array.from(element.querySelectorAll?.('[data-keyword], [data-value]') || []),
      ];
      return nodes.some((candidate) => [
        candidate?.getAttribute?.('data-keyword'),
        candidate?.getAttribute?.('data-value'),
      ].some((value) => String(value || '').trim().toLowerCase() === wanted));
    };
    const seen = new Set();
    const matches = [];
    const selectors = [
      '[role="option"]',
      '[role="menuitem"]',
      '[data-testid*="app"]',
      '[tabindex="0"]',
    ];
    for (const root of menuRoots) {
      for (const node of root.querySelectorAll(selectors.join(','))) {
        if (!visible(node) || input.contains(node) || seen.has(node)) continue;
        const text = [
          node.textContent,
          node.getAttribute('aria-label'),
          node.getAttribute('data-value'),
        ].filter(Boolean).join(' ').trim().toLowerCase();
        if (!keywordMatches(node) && !text.includes(wanted)) continue;
        seen.add(node);
        matches.push(node);
      }
    }
    return matches.sort((a, b) => {
      const score = (node) => {
        const role = node.getAttribute('role') || '';
        const testid = node.getAttribute('data-testid') || '';
        let value = 0;
        if (keywordMatches(node)) value += 100;
        if (role === 'option') value += 50;
        if (role === 'menuitem') value += 40;
        if (/app|connector|mention/i.test(testid)) value += 20;
        return value;
      };
      return score(b) - score(a);
    });
  }

  // Fixed-shape, non-sensitive App search diagnostics. Returns only booleans
  // and small counts derived from DOM structure; never page or draft text.
  // Used to discriminate "native suggestions never opened" from "opened in a
  // root this adapter does not scan" without loosening the identity contract.
  describeComposerAppSearch(keyword) {
    const wanted = String(keyword || '').trim().toLowerCase();
    const input = this.getInputEl();
    const visible = (element) => Boolean(element && (element.offsetWidth || element.offsetHeight || element.getClientRects?.().length));
    const count = (selector) => [...document.querySelectorAll(selector)]
      .filter((node) => visible(node) && !(input && input.contains(node))).length;
    const clamp = (n) => Math.min(Number(n) || 0, 50);
    const plain = input ? this.getComposerTextWithoutAppPills().toLowerCase() : '';
    const controlsId = input?.getAttribute?.('aria-controls') || input?.getAttribute?.('aria-owns') || '';
    const controlsTarget = controlsId ? document.getElementById(controlsId) : null;
    return {
      composer_found: Boolean(input),
      document_has_focus: typeof document.hasFocus === 'function' ? document.hasFocus() : false,
      document_visible: document.visibilityState === 'visible',
      composer_focused: Boolean(input && (input === document.activeElement || input.contains(document.activeElement))),
      composer_is_prompt_textarea: input?.id === 'prompt-textarea',
      search_text_committed: Boolean(wanted) && plain === `@${wanted}`,
      selected_pill_count: clamp(input ? input.querySelectorAll('[data-inline-selection-pill]').length : 0),
      inline_mention_count: clamp(input ? this.getInlineAppMentionNodes(input).length : 0),
      inline_mention_names: input ? this.getInlineAppMentionKeywords(input).slice(0, 5) : [],
      inline_mention_paths: input ? this.getInlineAppMentionNodes(input).slice(0, 3).map((n) => String(n.getAttribute('app-mention-path') || '').toLowerCase().replace(/[^a-z0-9_.:\/-]/g, '').slice(0, 80)) : [],
      composer_aria_expanded: input?.getAttribute?.('aria-expanded') === 'true',
      composer_aria_controls_present: Boolean(controlsId),
      composer_aria_controls_visible: visible(controlsTarget),
      visible_popover_roots: clamp(count('.popover')),
      visible_menu_roots: clamp(count('[role="menu"]')),
      visible_listbox_roots: clamp(count('[role="listbox"]')),
      visible_dialog_roots: clamp(count('[role="dialog"]')),
      visible_popper_wrappers: clamp(count('[data-radix-popper-content-wrapper]')),
      visible_option_nodes: clamp(count('[role="option"]')),
      visible_keyword_nodes: clamp([...document.querySelectorAll('[data-keyword]')]
        .filter((node) => visible(node) && !(input && input.contains(node))
          && String(node.getAttribute('data-keyword') || '').trim().toLowerCase() === wanted).length),
      candidate_count: clamp(input && wanted ? this.getComposerAppCandidates(wanted).length : 0),
      ...(() => {
        // Structure-only search for an unroled suggestion list: count visible
        // leaf-ish elements outside the composer whose own text is exactly the
        // keyword, and describe the first one's ancestors by tag/role/testid.
        if (!wanted) return {};
        const exact = [...document.querySelectorAll('body *')].filter((node) => {
          if (!visible(node) || (input && input.contains(node)) || node.children.length > 3) return false;
          return String(node.textContent || '').trim().toLowerCase() === wanted;
        });
        const chain = [];
        let cur = exact[0] || null;
        for (let i = 0; cur && i < 8; i += 1, cur = cur.parentElement) {
          const testid = String(cur.getAttribute('data-testid') || '').replace(/[^a-z0-9_-]/gi, '').slice(0, 40);
          chain.push([cur.tagName.toLowerCase(), cur.getAttribute('role') || '', testid,
            cur.hasAttribute('data-radix-popper-content-wrapper') ? 'popper' : '',
            /fixed|absolute/.test(getComputedStyle(cur).position) ? getComputedStyle(cur).position : ''].join('|'));
        }
        // Structure of an exact-keyword node inside the composer form but
        // outside the editor (a possible attachment chip): attribute names,
        // up to three class tokens, and whether aria-label equals the keyword.
        const form = input?.closest?.('form');
        const inForm = form ? exact.find((node) => form.contains(node)) : null;
        const formChain = [];
        let fc = inForm || null;
        for (let i = 0; fc && fc !== form && i < 7; i += 1, fc = fc.parentElement) {
          const names = [...fc.attributes].map((a) => a.name).filter((n) => /^[a-z-]{1,40}$/.test(n)).slice(0, 12);
          const classes = String(fc.getAttribute('class') || '').split(/\s+/).filter((c) => /^[A-Za-z0-9_:\[\]./%-]{1,40}$/.test(c)).slice(0, 3);
          formChain.push(`${fc.tagName.toLowerCase()}|${names.join(',')}|${classes.join(' ')}|${String(fc.getAttribute('aria-label') || '').trim().toLowerCase() === wanted ? 'label=kw' : ''}`);
        }
        // Editor-internal structure: tag, attribute names, and sanitized
        // values of identity-like data attributes only (no text content).
        const editorNodes = input ? [...input.querySelectorAll('*')].slice(0, 16).map((node) => {
          const names = [...node.attributes].map((a) => a.name).filter((n) => /^[a-z-]{1,40}$/.test(n)).slice(0, 10);
          const ids = [...node.attributes]
            .filter((a) => /^data-(keyword|symbol|type|id|mention|app|kind|node-type|entity)/.test(a.name))
            .map((a) => `${a.name}=${String(a.value || '').toLowerCase().replace(/[^a-z0-9_.:-]/g, '').slice(0, 40)}`)
            .slice(0, 6);
          return `${node.tagName.toLowerCase()}|${names.join(',')}|${ids.join(';')}|${String(node.textContent || '').trim().toLowerCase() === wanted ? 'kw' : ''}`;
        }) : [];
        return {
          editor_nodes: editorNodes,
          editor_contains_exact_keyword: Boolean(input && String(input.textContent || '').replace(/\uFEFF/g, '').trim().toLowerCase() === wanted),
          exact_keyword_form_chain: formChain,
          visible_exact_keyword_text_nodes: clamp(exact.length),
          exact_keyword_ancestor_chain: chain,
          exact_keyword_in_composer_form: Boolean(exact[0] && input?.closest?.('form')?.contains(exact[0])),
        };
      })(),
    };
  }

  getWatchMainWorldSelector() {
    const input = this.getInputEl();
    if (!input) return null;
    return input.id === 'prompt-textarea'
      ? '#prompt-textarea[contenteditable="true"]'
      : 'div.ProseMirror[role="textbox"][contenteditable="true"]';
  }

  getSendButtonCandidates() {
    const selectors = [
      'button[data-testid="send-button"]',
      'button[data-testid="composer-send-button"]',
      'button[aria-label="发送提示"]',
      'button[aria-label="Send prompt"]',
      'button[aria-label*="发送提示"]',
      'button[aria-label*="Send prompt" i]',
      'button[aria-label*="发送"]',
      'button[aria-label*="Send" i]',
    ];
    const seen = new Set();
    const out = [];
    for (const sel of selectors) {
      for (const el of document.querySelectorAll(sel)) {
        if (!seen.has(el)) { seen.add(el); out.push(el); }
      }
    }
    return out;
  }

  getSendButton() {
    return this.getSendButtonCandidates()[0] || null;
  }

  getStopButtonCandidates() {
    const input = this.getInputEl();
    const scope = input?.closest?.("form") || input?.parentElement?.parentElement || document;
    const selectors = [
      'button[data-testid="stop-button"]',
      '[role="button"][data-testid="stop-button"]',
      'button[aria-label="Stop generating" i]',
      'button[aria-label="Stop streaming" i]',
      'button[aria-label="停止生成"]',
      'button[aria-label="停止流式"]',
    ];
    const seen = new Set();
    const out = [];
    for (const selector of selectors) {
      for (const el of scope.querySelectorAll?.(selector) || []) {
        if (!seen.has(el)) { seen.add(el); out.push(el); }
      }
    }
    return out;
  }

  getComposerActionAnchor() {
    const input = this.getInputEl();
    const scope = input?.closest?.("form") || input?.parentElement?.parentElement || document;
    const selectors = [
      'button[data-testid="send-button"]',
      'button[data-testid="composer-send-button"]',
      'button[data-testid="stop-button"]',
      'button[aria-label*="Stop" i]',
      'button[aria-label*="停止"]',
      'button.composer-submit-button-color',
    ];
    for (const selector of selectors) {
      const button = scope.querySelector?.(selector);
      if (button) return button.closest?.("div.inline-flex") || button;
    }
    const button = this.getSendButton();
    return button?.closest?.("div.inline-flex") || button;
  }
}

registerH2WAdapter(new ChatGPTAdapter());
