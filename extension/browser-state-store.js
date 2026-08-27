import { normalizeBrowserState, applyBrowserEvent } from "./browser-state.js";

export class BrowserStateStore {
  constructor() {
    this.view = normalizeBrowserState();
    this.listeners = new Set();
  }

  replace(snapshot) {
    this.view = normalizeBrowserState(snapshot);
    this.emit();
  }

  apply(event) {
    this.view = applyBrowserEvent(this.view, event);
    this.emit();
  }

  subscribe(listener) {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  snapshot() {
    return this.view;
  }

  emit() {
    for (const listener of this.listeners) listener(this.view);
  }
}

export function createBrowserStateStore() {
  const store = new BrowserStateStore();
  return {
    get: () => store.snapshot(),
    snapshot: (value) => store.replace(value),
    event: (value) => store.apply(value),
    subscribe: (listener) => store.subscribe(listener),
  };
}
