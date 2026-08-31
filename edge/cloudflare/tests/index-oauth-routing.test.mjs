import test from "node:test";
import assert from "node:assert/strict";
import worker from "../dist/index.js";

const issuer = "https://herdr-mcp.agentforme.cc.cd";

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
  assert.equal(body.version, "0.4.3");
});
