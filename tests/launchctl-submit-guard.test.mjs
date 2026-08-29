import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

// Regression guard for the production UAT incident: an ad-hoc launchd
// submission invocation (the forbidden two-word command is joined
// programmatically below, never written literally) with label
// dev.herdr-mcp.rollback-uat.* running
// `<runtime>/herdr-mcp service rollback` created a launchd job that is
// keepalive-inferred ("inferred program"), so the destructive one-shot rollback
// action was re-run ~120 times, first consuming alpha.6 -> alpha.5, then
// alpha.5 -> alpha.1, then "no ready" repeats.
//
// The repository's production lifecycle code never schedules jobs through
// the forbidden submission command (it uses explicit plists + bootstrap/bootout, and the Rust
// service/update/native-host CLIs). These tests make that a durable, test-enforced
// invariant:
//   1. no executable lifecycle surface may ever invoke the forbidden command;
//   2. lifecycle docs may only mention it together with an explicit prohibition
//      and the safe one-shot invariant (explicit plist with RunAtLoad=true and
//      KeepAlive=false, or the managed CLI path).

const root = fileURLToPath(new URL('..', import.meta.url));
// Built at runtime so this test file itself never contains the literal pattern.
const FORBIDDEN = ['launchctl', 'submit'].join(' ');
const PROHIBITION = /must never|prohibited|禁止/;

// Surfaces that execute lifecycle mutations: schedule/mutation helpers, Rust
// service manager, Node server, CI, and tests.
const EXEC_SURFACES = ['bin', 'crates', 'src', 'scripts', '.github', 'extension', 'tests'];
// Landmarks allowed to *document* the pattern — but only with the prohibition.
const DOC_LANDMARKS = [
  'docs/herdr-architecture-roadmap.md',
  'docs/history/architecture/rust-native-rearchitecture.md',
  'docs/i18n/en/runtime-self-upgrade.md',
  'docs/i18n/zh-CN/runtime-self-upgrade.md',
  'AGENTS.md',
];

const SKIP_DIRS = new Set(['.git', 'node_modules', 'target', 'site-dist', 'dist']);
const BINARY_EXT = /\.(png|jpe?g|gif|webp|ico|woff2?|ttf|wasm|db|lock|p12)$/i;

async function walk(dir) {
  const out = [];
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    if (SKIP_DIRS.has(entry.name)) continue;
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...(await walk(full)));
    else out.push(full);
  }
  return out;
}

function relPath(file) {
  return file.slice(root.length).replace(/\\/g, '/');
}

function isSurface(rel) {
  return EXEC_SURFACES.some((s) => rel === s || rel.startsWith(`${s}/`));
}

test('lifecycle executable surfaces never invoke the forbidden launchd submission command', async () => {
  const files = (await walk(root)).filter(
    (f) => !BINARY_EXT.test(f) && !f.endsWith('Cargo.lock') && !f.endsWith('package-lock.json'),
  );
  const offenders = [];
  for (const file of files) {
    const rel = relPath(file);
    if (!isSurface(rel)) continue;
    const text = await readFile(file, 'utf8');
    if (text.includes(FORBIDDEN)) offenders.push(rel);
  }
  assert.deepEqual(
    offenders,
    [],
    `${FORBIDDEN} is forbidden for lifecycle mutations (keepalive-inferred jobs replay destructive one-shots); remove it from: ${offenders.join(', ')}`,
  );
});

test('lifecycle docs may mention the forbidden launchd submission command only with prohibition and safe one-shot invariant', async () => {
  const missingProhibition = [];
  const missingInvariant = [];
  for (const rel of DOC_LANDMARKS) {
    const text = await readFile(join(root, rel), 'utf8');
    if (!text.includes(FORBIDDEN)) continue;
    if (!PROHIBITION.test(text)) missingProhibition.push(rel);
    if (!/KeepAlive=false|KeepAlive.*false|keepalive.*replay|replay/i.test(text)) {
      missingInvariant.push(rel);
    }
  }
  assert.deepEqual(
    missingProhibition,
    [],
    `docs mentioning ${FORBIDDEN} must also prohibit it with ${PROHIBITION}: ${missingProhibition.join(', ')}`,
  );
  assert.deepEqual(
    missingInvariant,
    [],
    `docs mentioning ${FORBIDDEN} must state the safe one-shot invariant (KeepAlive=false / no keepalive replay): ${missingInvariant.join(', ')}`,
  );
});