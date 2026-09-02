import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const edgeScript = await readFile(new URL('../bin/herdr-cloudflare-token', import.meta.url), 'utf8');
const dnsScript = await readFile(new URL('../bin/herdr-cloudflare-dns-token', import.meta.url), 'utf8');
const tokenDoc = await readFile(new URL('../docs/i18n/en/cloudflare-edge-token.md', import.meta.url), 'utf8');
const deploymentDoc = await readFile(new URL('../docs/i18n/en/cloudflare-edge-deployment.md', import.meta.url), 'utf8');

test('long-lived Cloudflare token keeps Edge permissions separate from DNS migration', () => {
  assert.match(edgeScript, /Workers Routes Write/);
  assert.match(edgeScript, /Workers Scripts Write/);
  assert.doesNotMatch(edgeScript, /Workers R2 Storage Write/);
  assert.doesNotMatch(edgeScript, /\/r2\/buckets/);
  assert.doesNotMatch(edgeScript, /r2Access/);
  assert.match(edgeScript, /0o600/);
  assert.doesNotMatch(edgeScript, /DNS Write/);
  assert.doesNotMatch(edgeScript, /Tunnel Edit|Account Admin/);
});

test('long-lived token helper never promises to print the created secret', () => {
  assert.match(edgeScript, /Token values are never printed/);
  assert.doesNotMatch(edgeScript, /console\.log\([^\n]*created\.result\?\.value/i);
});

test('long-lived token helper supports dry-run, verify-only, rotation, and idempotency', () => {
  assert.match(edgeScript, /--dry-run/);
  assert.match(edgeScript, /--verify-only/);
  assert.match(edgeScript, /--rotate/);
  assert.match(edgeScript, /credential_already_ready/);
  assert.match(edgeScript, /credential_exists_use_rotate/);
});

test('ephemeral DNS cutover token is zone-scoped DNS Write only and mode 0600', () => {
  assert.match(dnsScript, /const DNS_WRITE = "DNS Write"/);
  assert.match(dnsScript, /com\.cloudflare\.api\.account\.zone\.\$\{resolved\.zoneId\}/);
  assert.match(dnsScript, /0o600/);
  assert.doesNotMatch(dnsScript, /Workers Routes Write|Workers Scripts Write|Zone DNS Settings Write/);
  assert.doesNotMatch(dnsScript, /4755a26eedb94da69e1066d98aa820be/);
});

test('ephemeral DNS token resolves permission dynamically and supports verify/revoke lifecycle', () => {
  assert.match(dnsScript, /g\?\.name === DNS_WRITE/);
  assert.match(dnsScript, /--dry-run/);
  assert.match(dnsScript, /--verify-only/);
  assert.match(dnsScript, /--revoke/);
  assert.match(dnsScript, /Generated token values are never printed/);
  assert.match(dnsScript, /dns_cutover_credential_revoked/);
});

test('Cloudflare docs describe workers.dev default, optional Custom Domain, and temporary DNS credential', () => {
  assert.match(tokenDoc, /bootstrap credential/);
  assert.match(tokenDoc, /Workers Routes Write/);
  assert.match(tokenDoc, /Workers Scripts Write/);
  assert.match(tokenDoc, /R2[^\n]*optional|optional[^\n]*R2/i);
  assert.match(tokenDoc, /0600/);
  assert.match(deploymentDoc, /does not require users to own a domain/);
  assert.match(deploymentDoc, /workers\.dev/);
  assert.match(deploymentDoc, /Custom Domain/);
  assert.match(deploymentDoc, /one-shot, target-zone-only `DNS Write` token/);
  assert.match(deploymentDoc, /herdr-cloudflare-dns-token --revoke/);
});
