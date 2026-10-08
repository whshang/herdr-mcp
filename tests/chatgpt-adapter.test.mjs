import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../extension/content/injector/chatgpt.js", import.meta.url), "utf8");
const legacySelector = '#prompt-textarea[contenteditable="true"]';
const projectSelector = 'div.ProseMirror[role="textbox"][contenteditable="true"]';

test("user can create a ChatGPT Project chat | Given an id-less composer | When the adapter locates it | Then only one visible editable textbox is accepted", () => {
  const selectors = new Map();
  let adapter;
  const document = {
    querySelector(selector) { return selectors.get(selector)?.[0] || null; },
    querySelectorAll(selector) { return selectors.get(selector) || []; },
  };
  class BaseAdapter {
    elementVisible(element) { return element?.visible === true; }
  }
  const registerH2WAdapter = (registered) => { adapter = registered; };
  vm.runInNewContext(source, { BaseAdapter, registerH2WAdapter, document }, {
    filename: "chatgpt.js",
  });

  const project = { id: "", visible: true };
  selectors.set(projectSelector, [project]);
  assert.equal(adapter.getInputEl(), project);
  assert.equal(adapter.getWatchMainWorldSelector(), projectSelector);

  const legacy = { id: "prompt-textarea", visible: true };
  selectors.set(legacySelector, [legacy]);
  assert.equal(adapter.getInputEl(), legacy);
  assert.equal(adapter.getWatchMainWorldSelector(), legacySelector);

  selectors.delete(legacySelector);
  selectors.set(projectSelector, [project, { id: "", visible: true }]);
  assert.equal(adapter.getInputEl(), null);
  assert.equal(adapter.getWatchMainWorldSelector(), null);

  selectors.set(projectSelector, [{ id: "", visible: false }]);
  assert.equal(adapter.getInputEl(), null);
});
