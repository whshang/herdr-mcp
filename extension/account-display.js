/** Display-only `<platform>-<email>` label. Not an authorization decision. */

const DISPLAY_PROVIDER = /^[a-z][a-z0-9]{0,31}$/;
const DISPLAY_EMAIL = /^[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,24}$/;

export function formatPlatformAccountDisplay(provider, email) {
  const platform = String(provider || "").trim().toLowerCase();
  const account = String(email || "").trim().toLowerCase();
  if (!DISPLAY_PROVIDER.test(platform) || !DISPLAY_EMAIL.test(account)) return null;
  const label = `${platform}-${account}`;
  return label.length <= 128 ? label : null;
}
