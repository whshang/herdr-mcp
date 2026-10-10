// Local methods whose runtime handler authorizes with Page Assist caller
// grants (endpoint_ref match). Mirrors the methods that receive
// `caller_page_assist_grants` in crates/herdr-mcp/src/mcp.rs dispatch and the
// constants in crates/herdr-mcp/src/progressive_skills.rs. The Edge forwards
// the caller's Page Assist grants only for these methods and only for the
// routed device; the runtime still enforces the endpoint match.
export const PAGE_SCOPED_LOCAL_METHODS: readonly string[] = Object.freeze([
  "herdr_mcp.page_assist",
  "herdr_mcp.browser_page.lifecycle",
  "herdr_mcp.browser_page.action",
  "herdr_mcp.browser_page.fast_path",
  "herdr_mcp.bilibili.video.transcript",
  "herdr_mcp.x.search.posts",
  "herdr_mcp.x.thread.read",
  "herdr_mcp.doubao.image.generate",
  "herdr_mcp.doubao.image.status",
]);

const PAGE_SCOPED_SET: ReadonlySet<string> = new Set(PAGE_SCOPED_LOCAL_METHODS);

export function isPageScopedLocalMethod(method: unknown): boolean {
  return typeof method === "string" && PAGE_SCOPED_SET.has(method);
}
