/**
 * Atomic multi-file apply for staged patch contents (all-or-nothing).
 */
import { writeFile, unlink, mkdir, rename, copyFile, open } from "node:fs/promises";
import * as path from "node:path";
import { randomBytes } from "node:crypto";

export type AtomicStage = {
  real: string;
  /** null = delete */
  content: string | null;
};

/**
 * Commit staged file writes/deletes with temp files + rename.
 * On failure after partial apply, restores from same-dir backups and removes
 * newly created files that had no prior backup.
 */
export async function commitAtomic(stages: AtomicStage[]): Promise<void> {
  if (!stages.length) return;
  const tag = randomBytes(4).toString("hex");
  const prepared: { real: string; tmp: string }[] = [];
  const backups: { real: string; backup: string }[] = [];
  /** Paths that did not exist before this commit (adds / moves dest). */
  const createdNew: string[] = [];

  try {
    for (const s of stages) {
      if (s.content === null) continue;
      await mkdir(path.dirname(s.real), { recursive: true });
      const tmp = path.join(path.dirname(s.real), `.herdr-mcp-patch-${tag}-${path.basename(s.real)}`);
      await writeFile(tmp, s.content, "utf-8");
      prepared.push({ real: s.real, tmp });
    }

    for (const s of stages) {
      try {
        const backup = path.join(path.dirname(s.real), `.herdr-mcp-bak-${tag}-${path.basename(s.real)}`);
        await copyFile(s.real, backup);
        backups.push({ real: s.real, backup });
      } catch {
        // missing => new file on apply
      }
    }
    const backed = new Set(backups.map((b) => b.real));

    for (const s of stages) {
      if (s.content === null) {
        await unlink(s.real);
      } else {
        const p = prepared.find((x) => x.real === s.real);
        if (!p) throw new Error(`missing prepared file for ${s.real}`);
        await rename(p.tmp, s.real);
        if (!backed.has(s.real)) createdNew.push(s.real);
      }
    }

    for (const b of backups) {
      try { await unlink(b.backup); } catch { /* ignore */ }
    }
  } catch (e) {
    for (const b of backups) {
      try { await rename(b.backup, b.real); } catch { /* leave backup */ }
    }
    for (const real of createdNew) {
      try { await unlink(real); } catch { /* ignore */ }
    }
    for (const p of prepared) {
      try { await unlink(p.tmp); } catch { /* ignore */ }
    }
    throw e;
  }
}

export async function fsyncPath(filePath: string): Promise<void> {
  const fh = await open(filePath, "r");
  try { await fh.sync(); } finally { await fh.close(); }
}
