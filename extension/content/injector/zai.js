// injector/zai.js — z.ai adapter, selectors and insertion verified on 2026-08-03
class ZaiAdapter extends BaseAdapter {
  get name() { return "z.ai"; }

  // The default textarea composer and host-plus-pathname identity are sufficient.
}

window.__H2W_ADAPTER__ = new ZaiAdapter();
