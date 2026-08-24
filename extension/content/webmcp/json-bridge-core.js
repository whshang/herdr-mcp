// json-bridge-core.js — pure helpers for browser-side Herdr JSON tool injection.
// Kept DOM/chrome-free so the protocol can be regression-tested in Node.
(function (root) {
  const MARKER = "[HERDR_JSON_BRIDGE_V1]";

  function scanToolCalls(text) {
    const source = String(text || "");
    const calls = [];
    let from = 0;
    let hasPrefix = false;
    let incomplete = false;
    let malformed = false;
    while (from < source.length) {
      const startMatch = /\{\s*"tool"\s*:/.exec(source.slice(from));
      const start = startMatch ? from + startMatch.index : -1;
      if (start === -1) break;
      hasPrefix = true;
      let depth = 0;
      let inString = false;
      let escape = false;
      let end = -1;
      for (let i = start; i < source.length; i++) {
        const c = source[i];
        if (escape) { escape = false; continue; }
        if (c === "\\") { escape = true; continue; }
        if (c === '"') { inString = !inString; continue; }
        if (inString) continue;
        if (c === "{") depth++;
        else if (c === "}") {
          depth--;
          if (depth === 0) { end = i + 1; break; }
        }
      }
      if (end === -1) {
        incomplete = true;
        break;
      }
      try {
        const parsed = JSON.parse(source.slice(start, end));
        if (parsed && typeof parsed.tool === "string" && parsed.tool.trim()) {
          const args = parsed.args && typeof parsed.args === "object" && !Array.isArray(parsed.args)
            ? parsed.args
            : {};
          calls.push({ tool: parsed.tool.trim(), args });
        } else {
          malformed = true;
        }
      } catch (_) { malformed = true; }
      from = end;
    }
    return { calls, hasPrefix, incomplete, malformed };
  }

  function extractToolCalls(text) {
    return scanToolCalls(text).calls;
  }

  function toolReplyState(text) {
    const scan = scanToolCalls(text);
    if (!scan.hasPrefix) {
      const trimmed = String(text || "")
        .replace(/^\s*```(?:json)?\s*/i, "")
        .trim();
      return /^[\[{]/.test(trimmed) ? "malformed" : "none";
    }
    if (scan.incomplete) return "incomplete";
    if (scan.malformed) return "malformed";
    return scan.calls.length ? "complete" : "malformed";
  }

  function hasPendingToolReply(entries) {
    const rows = Array.isArray(entries)
      ? entries.filter((entry) => entry && (entry.role === "user" || entry.role === "assistant"))
      : [];
    if (!rows.length) return false;
    const last = rows[rows.length - 1];
    if (last.role !== "assistant" || toolReplyState(last.text) === "none") return false;
    return rows.slice(Math.max(0, rows.length - 40), -1).some((entry) => {
      if (entry.role !== "user") return false;
      const text = String(entry.text || "");
      return text.includes(MARKER) || /^\s*TOOL_RESULT:/m.test(text);
    });
  }

  function schemaType(prop) {
    if (!prop || typeof prop !== "object") return "unknown";
    if (Array.isArray(prop.enum) && prop.enum.length) {
      return prop.enum.map((v) => JSON.stringify(v)).join(" | ");
    }
    if (Array.isArray(prop.anyOf) && prop.anyOf.length) return prop.anyOf.map(schemaType).join(" | ");
    if (Array.isArray(prop.oneOf) && prop.oneOf.length) return prop.oneOf.map(schemaType).join(" | ");
    if (Array.isArray(prop.type)) return prop.type.map((t) => schemaType({ ...prop, type: t })).join(" | ");
    if (prop.type === "array") return `${schemaType(prop.items)}[]`;
    if (prop.type === "object") return "Record<string, unknown>";
    if (prop.type === "string") return "string";
    if (prop.type === "integer" || prop.type === "number") return "number";
    if (prop.type === "boolean") return "boolean";
    if (prop.type === "null") return "null";
    return "unknown";
  }

  function safeDoc(text, max = 520) {
    return String(text || "")
      .replace(/\*\//g, "* /")
      .replace(/\s+/g, " ")
      .trim()
      .slice(0, max);
  }

  function propertyName(name) {
    return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name) ? name : JSON.stringify(name);
  }

  function schemaToTypedApi(tools) {
    const parts = ["declare const herdr: {"];
    for (const tool of Array.isArray(tools) ? tools : []) {
      if (!tool || typeof tool.name !== "string" || !tool.name.trim()) continue;
      const schema = tool.inputSchema && typeof tool.inputSchema === "object" ? tool.inputSchema : {};
      const props = schema.properties && typeof schema.properties === "object" ? schema.properties : {};
      const required = new Set(Array.isArray(schema.required) ? schema.required : []);
      const fields = Object.entries(props).map(([name, prop]) => (
        `${propertyName(name)}${required.has(name) ? "" : "?"}: ${schemaType(prop)}`
      ));
      const input = fields.length ? `{ ${fields.join("; ")} }` : "Record<string, never>";
      const description = safeDoc(tool.description || `Call ${tool.name}`);
      parts.push(`  /** ${description} */`);
      parts.push(`  ${propertyName(tool.name)}: (input: ${input}) => Promise<unknown>;`);
    }
    parts.push("};");
    return parts.join("\n");
  }

  function buildSystemPrompt(tools, siteName) {
    const siteHint = siteName === "deepseek"
      ? "\nDeepSeek renders $...$ as math. Inside JSON string arguments encode every literal dollar sign as \\u0024 so command text is preserved."
      : "";
    return `${MARKER}\nYou have local Herdr tools through a browser JSON bridge. Use them whenever the user asks you to inspect, modify, run, verify, or continue work on the bound workstation.\n\nTool-call protocol:\n- Emit one JSON object per line, exactly {\"tool\":\"<exact tool name>\",\"args\":{...}}.\n- When several calls are independent, emit multiple JSON lines in the same reply. They may run concurrently.\n- Keep dependent steps sequential.\n- A tool-call reply contains JSON only: no Markdown fences and no prose around the JSON.\n- Never invent tool names or arguments. Use the typed catalog below.\n- After a TOOL_RESULT message, either emit the next JSON tool call(s) or answer the user normally.\n- Do not claim a tool succeeded until its TOOL_RESULT confirms success.\n- Prefer targeted fs/git/exec operations over expensive agent prompts when deterministic tools can do the work.${siteHint}\n\nTools (typed API):\n${schemaToTypedApi(tools)}`;
  }

  function sanitizeToolResult(value, depth = 0) {
    if (depth > 7) return "[depth-limit]";
    if (value == null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") return value;
    if (Array.isArray(value)) return value.slice(0, 200).map((v) => sanitizeToolResult(v, depth + 1));
    if (typeof value !== "object") return String(value);
    const out = {};
    for (const [key, item] of Object.entries(value)) {
      if ((key === "data" || key === "blob" || key === "base64") && typeof item === "string" && item.length > 4096) {
        out[key] = `[binary omitted: ${item.length} chars]`;
        continue;
      }
      out[key] = sanitizeToolResult(item, depth + 1);
    }
    return out;
  }

  function formatToolResultBatch(calls, responses, maxChars = 60000) {
    const rows = calls.map((call, index) => {
      const response = responses[index] || { ok: false, error: "missing-tool-response" };
      const row = response.ok
        ? { index: index + 1, tool: call.tool, ok: true, result: sanitizeToolResult(response.result) }
        : { index: index + 1, tool: call.tool, ok: false, error: response.error || "tool-call-failed", detail: response.detail || "" };
      return row;
    });
    let text = JSON.stringify(rows);
    if (text.length > maxChars) text = `${text.slice(0, maxChars)}\n...[TOOL_RESULT truncated at ${maxChars} chars]`;
    return `TOOL_RESULT:\n${text}`;
  }

  root.H2W_JSON_BRIDGE_CORE = {
    MARKER,
    extractToolCalls,
    toolReplyState,
    hasPendingToolReply,
    schemaToTypedApi,
    buildSystemPrompt,
    sanitizeToolResult,
    formatToolResultBatch,
  };
})(globalThis);
