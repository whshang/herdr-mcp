/**
 * Apply-patch (*** Begin Patch *** envelope), compatible with coding-tools / Codex style.
 */
export class PatchError extends Error {
  code: string;
  details?: Record<string, unknown>;
  constructor(code: string, message: string, details?: Record<string, unknown>) {
    super(message);
    this.code = code;
    this.details = details;
  }
}

export type PatchOp =
  | { kind: "add"; path: string; content: string }
  | { kind: "delete"; path: string }
  | { kind: "update"; path: string; hunks: string[][]; move_to?: string };

export function parsePatch(patch: string): PatchOp[] {
  const lines = patch.split(/\r?\n/);
  if (!lines.length || lines[0].trim() !== "*** Begin Patch" || lines[lines.length - 1].trim() !== "*** End Patch") {
    throw new PatchError("PATCH_FAILED", "Patch must use *** Begin Patch / *** End Patch envelope.");
  }
  const ops: PatchOp[] = [];
  let i = 1;
  while (i < lines.length - 1) {
    const line = lines[i];
    if (!line) { i += 1; continue; }
    if (line.startsWith("*** Add File: ")) {
      const p = line.slice("*** Add File: ".length).trim();
      i += 1;
      const contentLines: string[] = [];
      while (i < lines.length - 1 && !lines[i].startsWith("*** ")) {
        if (!lines[i].startsWith("+")) {
          throw new PatchError("PATCH_FAILED", "Add file lines must start with '+'.");
        }
        contentLines.push(lines[i].slice(1));
        i += 1;
      }
      ops.push({ kind: "add", path: p, content: contentLines.join("\n") + "\n" });
      continue;
    }
    if (line.startsWith("*** Delete File: ")) {
      ops.push({ kind: "delete", path: line.slice("*** Delete File: ".length).trim() });
      i += 1;
      continue;
    }
    if (line.startsWith("*** Update File: ")) {
      const p = line.slice("*** Update File: ".length).trim();
      i += 1;
      let moveTo: string | undefined;
      if (i < lines.length - 1 && lines[i].startsWith("*** Move to: ")) {
        moveTo = lines[i].slice("*** Move to: ".length).trim();
        i += 1;
      }
      const hunks: string[][] = [];
      let current: string[] = [];
      while (i < lines.length - 1 && !lines[i].startsWith("*** ")) {
        if (lines[i].startsWith("@@")) {
          if (current.length) hunks.push(current);
          current = [];
        } else {
          current.push(lines[i]);
        }
        i += 1;
      }
      if (current.length) hunks.push(current);
      ops.push({ kind: "update", path: p, hunks, move_to: moveTo });
      continue;
    }
    throw new PatchError("PATCH_FAILED", `Unrecognized patch line: ${line}`);
  }
  return ops;
}

function parseUpdateHunk(hunk: string[]): { old: string[]; next: string[] } {
  const old: string[] = [];
  const next: string[] = [];
  for (const raw of hunk) {
    if (raw === "*** End of File") continue;
    if (!raw) throw new PatchError("PATCH_FAILED", "Invalid empty patch line.");
    const marker = raw[0];
    const value = marker === " " || marker === "-" || marker === "+" ? raw.slice(1) : raw;
    if (marker === " ") { old.push(value); next.push(value); }
    else if (marker === "-") old.push(value);
    else if (marker === "+") next.push(value);
    else throw new PatchError("PATCH_FAILED", "Update lines must start with space, '-' or '+'.");
  }
  return { old, next };
}

function findSubsequenceAll(lines: string[], needle: string[]): number[] {
  if (!needle.length) return [0];
  const limit = lines.length - needle.length + 1;
  const out: number[] = [];
  for (let i = 0; i < limit; i++) {
    if (lines[i] === needle[0] && lines.slice(i, i + needle.length).every((l, j) => l === needle[j])) {
      out.push(i);
    }
  }
  return out;
}

export function applyUpdateHunks(content: string, hunks: string[][], filePath: string): string {
  if (!hunks.length) return content;
  const bom = content.startsWith("\ufeff") ? "\ufeff" : "";
  const text = bom ? content.slice(1) : content;
  const crlf = text.includes("\r\n");
  const normalized = text.replace(/\r\n/g, "\n");
  const hadTrailing = normalized.endsWith("\n");
  const lines = normalized.split("\n");
  // splitlines drops final empty after trailing \n — match Python splitlines behavior
  if (hadTrailing && lines[lines.length - 1] === "") lines.pop();

  type Matched = { start: number; end: number; next: string[]; idx: number };
  const matched: Matched[] = [];
  for (let index = 0; index < hunks.length; index++) {
    const { old, next } = parseUpdateHunk(hunks[index]);
    const matches = old.length === 0 ? [0] : findSubsequenceAll(lines, old);
    if (!matches.length) {
      throw new PatchError("PATCH_CONTEXT_NOT_FOUND", `Patch context did not match in ${filePath}.`, {
        path: filePath, hunk_index: index, retry_hint: "Read the current file and regenerate this hunk.",
      });
    }
    if (matches.length > 1) {
      throw new PatchError("PATCH_CONTEXT_AMBIGUOUS", `Patch context matched ${matches.length} locations in ${filePath}.`, {
        path: filePath, hunk_index: index, match_count: matches.length,
        retry_hint: "Include additional unchanged context lines.",
      });
    }
    const start = matches[0];
    matched.push({ start, end: start + old.length, next, idx: index });
  }
  matched.sort((a, b) => a.start - b.start);
  for (let i = 0; i < matched.length - 1; i++) {
    if (matched[i].end > matched[i + 1].start) {
      throw new PatchError("PATCH_HUNKS_OVERLAP", `Patch hunks overlap in ${filePath}.`);
    }
  }
  let updated = [...lines];
  for (const m of [...matched].sort((a, b) => b.start - a.start)) {
    updated = [...updated.slice(0, m.start), ...m.next, ...updated.slice(m.end)];
  }
  let out = updated.join("\n");
  if (hadTrailing && (updated.length > 0 || out === "")) out += "\n";
  else if (!text && updated.length) out += "\n";
  if (crlf) out = out.replace(/\n/g, "\r\n");
  return bom + out;
}
