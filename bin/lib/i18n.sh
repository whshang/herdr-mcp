#!/usr/bin/env bash
# Shared i18n for herdr-mcp CLI (en / zh / ja). Sourced by bin/herdr-mcp.
# Config: ~/.config/herdr-mcp/ui.json  {"lang":"en"|"zh"|"ja"}
# First run: detect $LANG / $LC_ALL; unknown → en. Override: herdr-mcp lang <code>

HERDR_MCP_UI_CFG="${HERDR_MCP_UI_CFG:-$HOME/.config/herdr-mcp/ui.json}"

_herdr_mcp_detect_lang() {
  local raw="${LC_ALL:-${LANG:-en}}"
  raw=$(printf '%s' "$raw" | tr '[:upper:]' '[:lower:]' | cut -d. -f1 | tr '_' '-')
  case "$raw" in
    zh|zh-*|zh_*) echo zh ;;
    ja|ja-*|ja_*) echo ja ;;
    *) echo en ;;
  esac
}

_herdr_mcp_load_lang() {
  local stored=""
  if [[ -f "$HERDR_MCP_UI_CFG" ]]; then
    stored=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('lang',''))" "$HERDR_MCP_UI_CFG" 2>/dev/null || true)
  fi
  case "$stored" in
    en|zh|ja) HERDR_MCP_LANG="$stored" ;;
    *)
      HERDR_MCP_LANG="$(_herdr_mcp_detect_lang)"
      mkdir -p "$(dirname "$HERDR_MCP_UI_CFG")"
      printf '{"lang":"%s"}\n' "$HERDR_MCP_LANG" > "$HERDR_MCP_UI_CFG"
      ;;
  esac
}

herdr_mcp_set_lang() {
  local code="${1:-}"
  case "$code" in
    en|zh|ja) ;;
    *) echo "Usage: herdr-mcp lang <en|zh|ja>" >&2; return 1 ;;
  esac
  mkdir -p "$(dirname "$HERDR_MCP_UI_CFG")"
  printf '{"lang":"%s"}\n' "$code" > "$HERDR_MCP_UI_CFG"
  HERDR_MCP_LANG="$code"
  echo "lang=$code ($HERDR_MCP_UI_CFG)"
}

# T key → message for current HERDR_MCP_LANG (fallback en)
T() {
  local key="$1"
  case "$HERDR_MCP_LANG:$key" in
    # ---- English ----
    en:status_title) echo "herdr-mcp status" ;;
    en:proc_running) echo "process: running" ;;
    en:proc_stopped) echo "process: not running" ;;
    en:launchd_loaded) echo "launchd: loaded" ;;
    en:launchd_missing) echo "launchd: not loaded (will not auto-start)" ;;
    en:local_ok) echo "local MCP: OK" ;;
    en:local_401) echo "local MCP: 401 (token mismatch?)" ;;
    en:local_down) echo "local MCP: unreachable (not started?)" ;;
    en:local_http) echo "local MCP: HTTP" ;;
    en:public_unset) echo "public MCP: HERDR_MCP_BASE_URL not set (try Cloudflare Quick Tunnel)" ;;
    en:public_ok) echo "public MCP: reachable" ;;
    en:public_down) echo "public MCP: unreachable (tunnel down?)" ;;
    en:public_http) echo "public MCP: HTTP" ;;
    en:sock_ok) echo "herdr socket: present" ;;
    en:sock_missing) echo "herdr socket: missing — is herdr running?" ;;
    en:connector_title) echo "herdr-mcp connector info" ;;
    en:public_unset_line) echo "Public  : (HERDR_MCP_BASE_URL not set)" ;;
    en:recommend_tunnel) echo "Tip     : cloudflared tunnel --url http://127.0.0.1:8772" ;;
    en:config_docs) echo "Setup: README.md (default) / README.zh.md / README.ja.md" ;;
    en:already_running) echo "already running — use restart to reload code" ;;
    en:started) echo "started" ;;
    en:start_failed) echo "start failed — see: herdr-mcp logs" ;;
    en:stopped) echo "stopped" ;;
    en:stop_failed) echo "stop failed" ;;
    en:restarted) echo "restarted" ;;
    en:restart_failed) echo "restart failed — see: herdr-mcp logs" ;;
    en:no_log) echo "no log file:" ;;
    en:no_token) echo "token not configured (no HERDR_MCP_TOKEN in plist)" ;;
    en:copied) echo "(copied to clipboard)" ;;
    en:no_base_url) echo "HERDR_MCP_BASE_URL not set — see README (cloudflared tunnel)" ;;
    en:menu_title) echo "herdr-mcp  (herdr as MCP tool surface)" ;;
    en:menu_prompt) echo "  Press any key to exit, or 1-9 to continue:" ;;
    en:menu_1) echo "    1) Status" ;;
    en:menu_2) echo "    2) Connector info" ;;
    en:menu_3) echo "    3) Restart" ;;
    en:menu_4) echo "    4) Start" ;;
    en:menu_5) echo "    5) Stop" ;;
    en:menu_6) echo "    6) Logs (last 50 lines)" ;;
    en:menu_7) echo "    7) Copy Token" ;;
    en:menu_8) echo "    8) Copy public URL" ;;
    en:menu_9) echo "    9) Language (en/zh/ja)" ;;
    en:help_usage) echo "Usage: herdr-mcp [command]" ;;
    en:help_cmds) echo "Commands:" ;;
    en:help_menu) echo "  (no args)   interactive menu" ;;
    en:help_status) echo "  status      process / launchd / local / public / socket" ;;
    en:help_connector) echo "  connector   URL + Token + setup hints" ;;
    en:help_start) echo "  start       start (launchd)" ;;
    en:help_stop) echo "  stop        stop" ;;
    en:help_restart) echo "  restart     restart" ;;
    en:help_logs) echo "  logs [-f]   logs (follow)" ;;
    en:help_token) echo "  token       copy Token" ;;
    en:help_url) echo "  url         copy public URL" ;;
    en:help_lang) echo "  lang [code] show or set UI language (en|zh|ja)" ;;
    en:help_watchdog) echo "  watchdog    once|status|install|uninstall (MCP keep-alive + soft control-plane probe)" ;;
    en:unknown_cmd) echo "unknown command:" ;;
    en:lang_now) echo "current language:" ;;

    # ---- Simplified Chinese ----
    zh:status_title) echo "herdr-mcp 状态" ;;
    zh:proc_running) echo "进程: 运行中" ;;
    zh:proc_stopped) echo "进程: 未运行" ;;
    zh:launchd_loaded) echo "launchd: 已加载" ;;
    zh:launchd_missing) echo "launchd: 未加载 (服务不会自动启动)" ;;
    zh:local_ok) echo "本地 MCP: 响应正常" ;;
    zh:local_401) echo "本地 MCP: 401 (token 不匹配?)" ;;
    zh:local_down) echo "本地 MCP: 不可达 (服务未启动?)" ;;
    zh:local_http) echo "本地 MCP: HTTP" ;;
    zh:public_unset) echo "公网 MCP: 未配置 HERDR_MCP_BASE_URL (推荐 Cloudflare Quick Tunnel)" ;;
    zh:public_ok) echo "公网 MCP: 可达" ;;
    zh:public_down) echo "公网 MCP: 不可达 (tunnel 未跑?)" ;;
    zh:public_http) echo "公网 MCP: HTTP" ;;
    zh:sock_ok) echo "herdr socket: 存在" ;;
    zh:sock_missing) echo "herdr socket: 不存在 — herdr 没在跑?" ;;
    zh:connector_title) echo "herdr-mcp 接入信息" ;;
    zh:public_unset_line) echo "Public  : (未配置 HERDR_MCP_BASE_URL)" ;;
    zh:recommend_tunnel) echo "推荐    : cloudflared tunnel --url http://127.0.0.1:8772" ;;
    zh:config_docs) echo "配置步骤见 README.md / README.zh.md / README.ja.md" ;;
    zh:already_running) echo "已在运行 — 如需加载新代码请用 restart" ;;
    zh:started) echo "已启动" ;;
    zh:start_failed) echo "启动失败 — 查看: herdr-mcp logs" ;;
    zh:stopped) echo "已停止" ;;
    zh:stop_failed) echo "停止失败" ;;
    zh:restarted) echo "已重启" ;;
    zh:restart_failed) echo "重启失败 — 查看: herdr-mcp logs" ;;
    zh:no_log) echo "无日志文件:" ;;
    zh:no_token) echo "未配置 token (plist 里没有 HERDR_MCP_TOKEN)" ;;
    zh:copied) echo "(已复制到剪贴板)" ;;
    zh:no_base_url) echo "未配置 HERDR_MCP_BASE_URL — 见 README（cloudflared tunnel）" ;;
    zh:menu_title) echo "herdr-mcp  (herdr 作为 MCP 工具面)" ;;
    zh:menu_prompt) echo "  按任意键退出，或输入 1-9 继续:" ;;
    zh:menu_1) echo "    1) 查看状态" ;;
    zh:menu_2) echo "    2) 查看接入信息" ;;
    zh:menu_3) echo "    3) 重启服务" ;;
    zh:menu_4) echo "    4) 启动服务" ;;
    zh:menu_5) echo "    5) 停止服务" ;;
    zh:menu_6) echo "    6) 查看日志 (最近 50 行)" ;;
    zh:menu_7) echo "    7) 复制 Token" ;;
    zh:menu_8) echo "    8) 复制公网 URL" ;;
    zh:menu_9) echo "    9) 语言 (en/zh/ja)" ;;
    zh:help_usage) echo "Usage: herdr-mcp [command]" ;;
    zh:help_cmds) echo "Commands:" ;;
    zh:help_menu) echo "  (no args)   交互菜单" ;;
    zh:help_status) echo "  status      状态 (进程/launchd/本地/公网/socket)" ;;
    zh:help_connector) echo "  connector   接入信息 (URL + Token)" ;;
    zh:help_start) echo "  start       启动 (launchd)" ;;
    zh:help_stop) echo "  stop        停止" ;;
    zh:help_restart) echo "  restart     重启" ;;
    zh:help_logs) echo "  logs [-f]   日志 (跟随)" ;;
    zh:help_token) echo "  token       复制 Token" ;;
    zh:help_url) echo "  url         复制公网 URL" ;;
    zh:help_lang) echo "  lang [code] 查看或设置界面语言 (en|zh|ja)" ;;
    zh:help_watchdog) echo "  watchdog    once|status|install|uninstall (MCP 保活 + 控制面软探测)" ;;
    zh:unknown_cmd) echo "未知命令:" ;;
    zh:lang_now) echo "当前语言:" ;;

    # ---- Japanese ----
    ja:status_title) echo "herdr-mcp ステータス" ;;
    ja:proc_running) echo "プロセス: 実行中" ;;
    ja:proc_stopped) echo "プロセス: 停止中" ;;
    ja:launchd_loaded) echo "launchd: 読み込み済み" ;;
    ja:launchd_missing) echo "launchd: 未読み込み（自動起動しません）" ;;
    ja:local_ok) echo "ローカル MCP: OK" ;;
    ja:local_401) echo "ローカル MCP: 401（token 不一致?）" ;;
    ja:local_down) echo "ローカル MCP: 到達不可（未起動?）" ;;
    ja:local_http) echo "ローカル MCP: HTTP" ;;
    ja:public_unset) echo "公開 MCP: HERDR_MCP_BASE_URL 未設定（Cloudflare Quick Tunnel 推奨）" ;;
    ja:public_ok) echo "公開 MCP: 到達可" ;;
    ja:public_down) echo "公開 MCP: 到達不可（tunnel 停止?）" ;;
    ja:public_http) echo "公開 MCP: HTTP" ;;
    ja:sock_ok) echo "herdr socket: あり" ;;
    ja:sock_missing) echo "herdr socket: なし — herdr は起動していますか?" ;;
    ja:connector_title) echo "herdr-mcp 接続情報" ;;
    ja:public_unset_line) echo "Public  : (HERDR_MCP_BASE_URL 未設定)" ;;
    ja:recommend_tunnel) echo "推奨    : cloudflared tunnel --url http://127.0.0.1:8772" ;;
    ja:config_docs) echo "手順: README.md / README.zh.md / README.ja.md" ;;
    ja:already_running) echo "既に実行中 — コード再読込は restart" ;;
    ja:started) echo "起動しました" ;;
    ja:start_failed) echo "起動失敗 — herdr-mcp logs を確認" ;;
    ja:stopped) echo "停止しました" ;;
    ja:stop_failed) echo "停止失敗" ;;
    ja:restarted) echo "再起動しました" ;;
    ja:restart_failed) echo "再起動失敗 — herdr-mcp logs を確認" ;;
    ja:no_log) echo "ログファイルなし:" ;;
    ja:no_token) echo "token 未設定（plist に HERDR_MCP_TOKEN なし）" ;;
    ja:copied) echo "（クリップボードにコピー済み）" ;;
    ja:no_base_url) echo "HERDR_MCP_BASE_URL 未設定 — README（cloudflared tunnel）を参照" ;;
    ja:menu_title) echo "herdr-mcp  (herdr を MCP ツール面に)" ;;
    ja:menu_prompt) echo "  任意キーで終了、または 1-9 で続行:" ;;
    ja:menu_1) echo "    1) ステータス" ;;
    ja:menu_2) echo "    2) 接続情報" ;;
    ja:menu_3) echo "    3) 再起動" ;;
    ja:menu_4) echo "    4) 起動" ;;
    ja:menu_5) echo "    5) 停止" ;;
    ja:menu_6) echo "    6) ログ（直近 50 行）" ;;
    ja:menu_7) echo "    7) Token をコピー" ;;
    ja:menu_8) echo "    8) 公開 URL をコピー" ;;
    ja:menu_9) echo "    9) 言語 (en/zh/ja)" ;;
    ja:help_usage) echo "Usage: herdr-mcp [command]" ;;
    ja:help_cmds) echo "Commands:" ;;
    ja:help_menu) echo "  (no args)   対話メニュー" ;;
    ja:help_status) echo "  status      プロセス / launchd / ローカル / 公開 / socket" ;;
    ja:help_connector) echo "  connector   URL + Token" ;;
    ja:help_start) echo "  start       起動 (launchd)" ;;
    ja:help_stop) echo "  stop        停止" ;;
    ja:help_restart) echo "  restart     再起動" ;;
    ja:help_logs) echo "  logs [-f]   ログ（フォロー）" ;;
    ja:help_token) echo "  token       Token をコピー" ;;
    ja:help_url) echo "  url         公開 URL をコピー" ;;
    ja:help_lang) echo "  lang [code] UI 言語の表示/設定 (en|zh|ja)" ;;
    ja:help_watchdog) echo "  watchdog    once|status|install|uninstall (MCP 生存 + 制御面ソフト検査)" ;;
    ja:unknown_cmd) echo "不明なコマンド:" ;;
    ja:lang_now) echo "現在の言語:" ;;

    *)
      if [[ "${HERDR_MCP_LANG}" != "en" ]]; then
        local _saved="$HERDR_MCP_LANG"
        HERDR_MCP_LANG=en
        T "$key"
        HERDR_MCP_LANG="$_saved"
      else
        printf '%s' "$key"
      fi
      ;;
  esac
}

_herdr_mcp_load_lang
