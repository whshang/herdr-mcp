// #586 old <-> new Runtime/Extension negotiation, Extension -> older Runtime.
//
// The "older Runtime" peer in this test is not a hand-written assumption: its
// accepted field set is parsed from the real `v1.1.0-rc.10` source
// (`browser_registry_allow_fields` in `crates/herdr-mcp/src/mcp_http.rs`) and
// its capability vocabulary from `ALLOWED_BROWSER_CAPABILITY_OPERATIONS` in
// `state_store.rs`. If a future Runtime drops a field, or the running Extension
// starts declaring an operation the released peer rejects, these tests fail
// here instead of in a field rollback.
//
// The Extension side under test is the real shipped code in
// `extension/background.js` (`sourceTurnEndpointProvenance`,
// `postSourceTurnObserve`, `browserEndpointView`); only the transport boundary
// (`postBrowserRegistry`) is a contract-checked fake.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  capabilityOperations,
  gitShow,
  registryAllowLists,
} from "../scripts/compat/registry-contract.mjs";

const RC10_TAG = "v1.1.0-rc.10";
const repoRoot = fileURLToPath(new URL("..", import.meta.url));
const backgroundSource = readFileSync(new URL("../extension/background.js", import.meta.url), "utf8");

const rc10McpHttp = gitShow(repoRoot, RC10_TAG, "crates/herdr-mcp/src/mcp_http.rs");
const rc10StateStore = gitShow(repoRoot, RC10_TAG, "crates/herdr-mcp/src/state_store.rs");
const rc10Registry = rc10McpHttp ? registryAllowLists(rc10McpHttp) : null;
const rc10Capabilities = rc10StateStore ? capabilityOperations(rc10StateStore) : null;
const skipReason = rc10McpHttp && rc10StateStore
  ? false
  : `needs the ${RC10_TAG} tag in this checkout (git show ${RC10_TAG}:crates/herdr-mcp/src/...)`;

function slice(startMarker, endMarker, what) {
  const start = backgroundSource.indexOf(startMarker);
  const end = backgroundSource.indexOf(endMarker, start);
  assert.ok(start >= 0 && end > start, `${what} must remain extractable`);
  return backgroundSource.slice(start, end);
}

// The shipped Extension path for one source turn, wired to a peer whose
// registry allow-list comes from the released tag.
function extensionUnderTest(peer) {
  const provenanceSource = slice(
    "async function sourceTurnEndpointProvenance(",
    "\n/**\n * #734: post one source turn.",
    "sourceTurnEndpointProvenance",
  );
  const postSourceTurnSource = slice(
    "async function postSourceTurnObserve(",
    "\nfunction heldSourceTurnKey(",
    "postSourceTurnObserve",
  );
  const seedGuardSource = slice(
    "function validBrowserProfileSeed(",
    "async function getOrCreateBrowserProfileSeed(",
    "validBrowserProfileSeed",
  );
  const endpointViewSource = slice(
    "function browserEndpointView(",
    "\nasync function setLocalBrowserWebchatControlConsent(",
    "browserEndpointView",
  );
  return new Function(
    "peer",
    `
    let sourceTurnProvenanceUnsupported = false;
    let browserEndpoint = null;
    const logs = [];
    const callLog = (...args) => logs.push(args.join(" "));
    const getOrCreateBrowserProfileSeed = async () => peer.profileSeed();
    ${seedGuardSource}
    ${endpointViewSource}
    ${provenanceSource}
    const postBrowserRegistry = async (payload) => peer.post(payload);
    ${postSourceTurnSource}
    return {
      postSourceTurnObserve,
      sourceTurnEndpointProvenance,
      logs,
      setEndpoint(value) { browserEndpoint = value; },
      get provenanceUnsupported() { return sourceTurnProvenanceUnsupported; },
    };
  `,
  )(peer);
}

// A peer that implements exactly one released runtime's registry contract.
function releasedPeer({ allowList, profileSeed, endpointRef }) {
  const requests = [];
  let failure = null;
  return {
    allowList,
    requests,
    profileSeed: async () => profileSeed(),
    endpointRef,
    failWith(code) {
      failure = code;
    },
    async post(payload) {
      requests.push({ ...payload });
      const allowed = allowList.get(payload.operation);
      assert.ok(allowed, `the released runtime must know ${payload.operation}`);
      const unexpected = Object.keys(payload).filter((key) => !allowed.includes(key));
      if (unexpected.length > 0) {
        // Exactly the released runtime's rejection for unknown fields.
        const error = new Error("browser_registry_params_invalid");
        error.status = 400;
        throw error;
      }
      if (failure) {
        const error = new Error(failure);
        error.status = 400;
        throw error;
      }
      return { ok: true };
    },
  };
}

const PROFILE_SEED = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const ENDPOINT_REF = "bep_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

function sourceTurnClock() {
  let observedAt = 1_700_000_000_000;
  return () => {
    observedAt += 1;
    return observedAt;
  };
}

test(
  "user keeps source-turn binding on an rc.10 Runtime | Given the registry allow-list that v1.1.0-rc.10 actually ships, which rejects the endpoint provenance keys as browser_registry_params_invalid | When the shipped Extension observes a user turn | Then it spends exactly one rejected attempt and then posts the legacy four-field payload unchanged",
  { skip: skipReason },
  async () => {
    assert.deepEqual(
      [...rc10Registry.get("source_turn.observe")].sort(),
      ["canonical_url", "observed_at", "operation", "user_text"],
      "rc.10 must admit exactly the legacy four-field source turn",
    );
    const peer = releasedPeer({ allowList: rc10Registry, profileSeed: () => PROFILE_SEED, endpointRef: ENDPOINT_REF });
    const extension = extensionUnderTest(peer);
    extension.setEndpoint({ endpoint_ref: ENDPOINT_REF, consent_revision: 1 });

    const sourceTurn = {
      operation: "source_turn.observe",
      canonical_url: "https://chatgpt.com/c/rc10-compat-source-turn",
      user_text: "旧 Runtime 只接受四个字段",
      observed_at: 1_700_000_000_123,
    };
    await extension.postSourceTurnObserve({
      sourceTurn,
      provenance: await extension.sourceTurnEndpointProvenance(),
    });

    assert.equal(peer.requests.length, 2, JSON.stringify(peer.requests));
    assert.deepEqual(
      Object.keys(peer.requests[0]).sort(),
      ["canonical_url", "endpoint_ref", "observed_at", "operation", "profile_seed", "user_text"],
      "the first attempt must report this profile's endpoint provenance",
    );
    assert.deepEqual(peer.requests[1], sourceTurn, "the retry must be the legacy payload, replayed unchanged");
    assert.equal(extension.provenanceUnsupported, true, "the peer must be remembered for this worker lifetime");
    assert.ok(
      extension.logs.some((line) => line.includes("has no endpoint provenance support")),
      "the fallback must be logged for diagnosis",
    );
  },
);

test(
  "user pays no repeated rejected attempt | Given an rc.10 Runtime that already refused the provenance keys once | When a later turn starts | Then the very first observe is already the legacy four-field payload",
  { skip: skipReason },
  async () => {
    const peer = releasedPeer({ allowList: rc10Registry, profileSeed: () => PROFILE_SEED, endpointRef: ENDPOINT_REF });
    const extension = extensionUnderTest(peer);
    extension.setEndpoint({ endpoint_ref: ENDPOINT_REF, consent_revision: 1 });
    await extension.postSourceTurnObserve({
      sourceTurn: { operation: "source_turn.observe", canonical_url: "https://chatgpt.com/c/a", user_text: "first", observed_at: 1 },
      provenance: await extension.sourceTurnEndpointProvenance(),
    });
    assert.equal(peer.requests.length, 2, "the first turn must exercise the rejected provenance attempt");
    peer.requests.length = 0;

    await extension.postSourceTurnObserve({
      sourceTurn: { operation: "source_turn.observe", canonical_url: "https://chatgpt.com/c/b", user_text: "second", observed_at: 2 },
      provenance: await extension.sourceTurnEndpointProvenance(),
    });

    assert.equal(peer.requests.length, 1, JSON.stringify(peer.requests));
    assert.deepEqual(
      Object.keys(peer.requests[0]).sort(),
      ["canonical_url", "observed_at", "operation", "user_text"],
      "no provenance keys may be offered again in this worker lifetime",
    );
  },
);

test(
  "user keeps fail-closed delivery when provenance is accepted but the turn is rejected | Given a Runtime whose registry contract already admits the endpoint provenance keys | When the source-turn observe fails with any code other than browser_registry_params_invalid | Then the rejection is rethrown without a second payload and provenance stays enabled",
  async () => {
    const currentRegistry = registryAllowLists(
      readFileSync(new URL("../crates/herdr-mcp/src/mcp_http.rs", import.meta.url), "utf8"),
    );
    const peer = releasedPeer({ allowList: currentRegistry, profileSeed: () => PROFILE_SEED, endpointRef: ENDPOINT_REF });
    peer.failWith("browser_source_session_not_found");
    const extension = extensionUnderTest(peer);
    extension.setEndpoint({ endpoint_ref: ENDPOINT_REF, consent_revision: 1 });

    await assert.rejects(
      extension.postSourceTurnObserve({
        sourceTurn: { operation: "source_turn.observe", canonical_url: "https://chatgpt.com/c/c", user_text: "third", observed_at: 3 },
        provenance: await extension.sourceTurnEndpointProvenance(),
      }),
      /browser_source_session_not_found/,
    );
    assert.equal(peer.requests.length, 1, "an untyped failure must never be replayed");
    assert.deepEqual(
      Object.keys(peer.requests[0]).sort(),
      ["canonical_url", "endpoint_ref", "observed_at", "operation", "profile_seed", "user_text"],
      "the attempt under test must be the provenance attempt",
    );
    assert.equal(extension.provenanceUnsupported, false, "a non-allow-list rejection must not disable provenance");
  },
);

test(
  "user keeps a released Runtime usable with the current Extension | Given the capability vocabulary of v1.1.0-rc.10 and the operations the shipped Extension declares | When the Extension reports provider capabilities | Then every declared operation is already accepted by that released Runtime",
  { skip: skipReason },
  async () => {
    const capabilitiesSource = slice(
      "function browserProviderCapabilities(provider)",
      "\nasync function observeBrowserConversation(",
      "browserProviderCapabilities",
    );
    // eslint-disable-next-line no-new-func
    const browserProviderCapabilities = new Function(`${capabilitiesSource}; return browserProviderCapabilities;`)();
    const declared = new Set();
    for (const provider of ["chatgpt", "claude", "gemini", "grok"]) {
      const capabilities = browserProviderCapabilities(provider);
      assert.equal(capabilities.schema_version, 1, "the capability schema version must stay at the value rc.10 knows");
      for (const operation of capabilities.operations) declared.add(operation);
    }
    const unknown = [...declared].filter((operation) => !rc10Capabilities.includes(operation));
    assert.deepEqual(unknown, [], "rc.10 would reject provider.observe with browser_capabilities_invalid");
  },
);
