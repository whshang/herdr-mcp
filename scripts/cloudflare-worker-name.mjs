#!/usr/bin/env node
import { createHash } from "node:crypto";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const CLOUDFLARE_WORKER_NAME_MAX = 63;
export const HERDR_WORKER_PREFIX = "herdr-edge-";

function shortHash(value) {
  return createHash("sha256").update(String(value ?? ""), "utf8").digest("hex").slice(0, 10);
}

export function cloudflareMachineSlug(value) {
  const source = String(value ?? "").trim();
  const maxSlugLength = CLOUDFLARE_WORKER_NAME_MAX - HERDR_WORKER_PREFIX.length;
  let slug = source
    .normalize("NFKD")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-+|-+$/g, "");

  if (!slug) slug = `host-${shortHash(source)}`;
  if (slug.length > maxSlugLength) {
    const suffix = shortHash(source);
    const headLength = Math.max(1, maxSlugLength - suffix.length - 1);
    const head = slug.slice(0, headLength).replace(/-+$/g, "") || "host";
    slug = `${head}-${suffix}`;
  }
  slug = slug.slice(0, maxSlugLength).replace(/^-+|-+$/g, "");
  if (!slug) slug = `host-${shortHash(source)}`.slice(0, maxSlugLength).replace(/-+$/g, "");
  return slug;
}

export function cloudflareWorkerName(value) {
  const name = `${HERDR_WORKER_PREFIX}${cloudflareMachineSlug(value)}`;
  if (name.length > CLOUDFLARE_WORKER_NAME_MAX
    || !/^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/.test(name)) {
    throw new Error(`invalid Cloudflare Worker name: ${name}`);
  }
  return name;
}

const isMain = process.argv[1]
  && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url));

if (isMain) {
  const source = process.argv.slice(2).join(" ").trim();
  if (!source) {
    console.error("usage: node scripts/cloudflare-worker-name.mjs <hostname>");
    process.exitCode = 2;
  } else {
    console.log(cloudflareWorkerName(source));
  }
}
