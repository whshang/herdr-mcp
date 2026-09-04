import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import worker from "../dist/index.js";

const issuer = "https://herdr-mcp.agentforme.cc.cd";
const packageJson = JSON.parse(
  readFileSync(new URL("../../../package.json", import.meta.url), "utf8"),
);

function env() {
  const stub = { fetch: async () => new Response(JSON.stringify({ ok: false, code: "unused" }), { status: 500 }) };
  return {
    OAUTH_ISSUER: issuer,
    OAUTH_STORE_DO: {
      idFromName(name) { return name; },
      get() { return stub; },
    },
  };
}

test("worker routes OAuth discovery before the generic /.well-known fallback", async () => {
  const response = await worker.fetch(
    new Request("https://herdr-edge-dev.example/.well-known/oauth-authorization-server"),
    env(),
  );
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.issuer, issuer);
  assert.equal(body.authorization_endpoint, `${issuer}/oauth/authorize`);
  assert.equal(body.token_endpoint, `${issuer}/oauth/token`);
  assert.equal(body.registration_endpoint, `${issuer}/oauth/register`);
});

test("worker routes MCP server card before the generic /.well-known fallback", async () => {
  const response = await worker.fetch(
    new Request("https://herdr-edge-dev.example/.well-known/mcp.json"),
    env(),
  );
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.equal(body.serverUrl, `${issuer}/mcp`);
  assert.equal(body.name, "herdr-mcp");
  assert.equal(body.version, packageJson.version);
});

test("OAuth authorization fails closed when the short-code HMAC secret is unavailable", async () => {
  const response = await worker.fetch(
    new Request(`${issuer}/oauth/authorize?client_id=anything`),
    env(),
  );
  assert.equal(response.status, 503);
  const body = await response.json();
  assert.equal(body.error, "server_error");
  assert.equal(body.error_description, "OAuth owner approval is not configured");
});
