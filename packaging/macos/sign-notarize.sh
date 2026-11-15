#!/usr/bin/env bash
#
# srmgui macOS 签名 / 公证脚本
#
# 用法：
#   ./packaging/macos/sign-notarize.sh <srmgui.app> [输出 dmg 路径]
#
# 必填环境变量：
#   MACOS_SIGN_IDENTITY   Developer ID Application 证书名称，
#                         例：Developer ID Application: Your Name (TEAMID1234)
#
# 公证凭据（三选一，缺省时仅签名不公证）：
#   NOTARY_PROFILE        xcrun notarytool store-credentials 保存的 profile 名
#   APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD
#                          Apple ID + Team ID + App 专用密码
#   AC_API_KEY_PATH + AC_API_KEY_ID + AC_API_ISSUER_ID
#                          App Store Connect API Key（.p8）方式
#
# 示例：
#   export MACOS_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID1234)"
#   export APPLE_ID="you@example.com"
#   export APPLE_TEAM_ID="TEAMID1234"
#   export APPLE_APP_PASSWORD="abcd-efgh-ijkl-mnop"
#   ./packaging/macos/sign-notarize.sh bundle/srmgui.app dist/srmgui-1.2.1-macos-arm64.dmg
#
set -euo pipefail

APP_PATH="${1:-}"
DMG_PATH="${2:-}"
ENTITLEMENTS="${MACOS_ENTITLEMENTS:-packaging/macos/entitlements.plist}"

log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[warn]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[error]\033[0m %s\n' "$*" >&2; exit 1; }

[ -n "$APP_PATH" ] || die "用法: $0 <srmgui.app> [输出 dmg 路径]"
[ -d "$APP_PATH" ] || die "找不到 .app: $APP_PATH"

APP_NAME="$(basename "$APP_PATH" .app)"
BIN_PATH="$APP_PATH/Contents/MacOS/$APP_NAME"
[ -f "$BIN_PATH" ] || die "找不到可执行文件: $BIN_PATH"

[ -n "${MACOS_SIGN_IDENTITY:-}" ] || die "未设置 MACOS_SIGN_IDENTITY（Developer ID Application 证书名称）"

# 读取有效签名身份；命令失败（如钥匙串未解锁）与「无匹配证书」要区分开
if ! IDENTITIES="$(security find-identity -v -p codesigning 2>&1)"; then
  die "读取签名身份失败，请确认钥匙串已解锁（security unlock-keychain）"
fi

IDENTITY_NAMES="$(printf '%s\n' "$IDENTITIES" \
  | sed -n 's/^[[:space:]]*[0-9][0-9]*) [0-9A-Fa-f]* "\(.*\)"$/\1/p')"

# 精确匹配身份名称，避免前缀相同的证书被误签
# 这里不能用 grep -q：pipefail 下提前退出会让 printf 收到 SIGPIPE
if ! printf '%s\n' "$IDENTITY_NAMES" | grep -Fx -- "$MACOS_SIGN_IDENTITY" >/dev/null; then
  die "钥匙串中找不到签名证书: $MACOS_SIGN_IDENTITY"
fi

case "$MACOS_SIGN_IDENTITY" in
  "Developer ID Application:"*) ;;
  *) warn "该证书不是 Developer ID Application，产物无法通过公证" ;;
esac

SIGN_ARGS=(--force --timestamp --options runtime --sign "$MACOS_SIGN_IDENTITY")
if [ -f "$ENTITLEMENTS" ]; then
  SIGN_ARGS+=(--entitlements "$ENTITLEMENTS")
  log "使用 entitlements: $ENTITLEMENTS"
fi

# 1. 先签可执行文件，再签整个 bundle（由内向外）
log "签名可执行文件: $BIN_PATH"
codesign "${SIGN_ARGS[@]}" "$BIN_PATH"

log "签名 app bundle: $APP_PATH"
codesign "${SIGN_ARGS[@]}" "$APP_PATH"

log "校验签名"
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
codesign -dv --verbose=4 "$APP_PATH" 2>&1 | grep -E 'Authority|TeamIdentifier|Timestamp' || true

if [ -z "$DMG_PATH" ]; then
  DMG_PATH="dist/$APP_NAME.dmg"
fi
mkdir -p "$(dirname "$DMG_PATH")"

# 2. 生成 dmg 并对其签名
log "生成 dmg: $DMG_PATH"
rm -f "$DMG_PATH"
hdiutil create -volname "$APP_NAME" -srcfolder "$APP_PATH" -ov -format UDZO "$DMG_PATH"
# 磁盘镜像不是 Mach-O，不能带 --options runtime，仅做普通签名
codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$DMG_PATH"
codesign --verify --verbose=2 "$DMG_PATH"

# 3. 公证
NOTARY_ARGS=()
if [ -n "${NOTARY_PROFILE:-}" ]; then
  NOTARY_ARGS=(--keychain-profile "$NOTARY_PROFILE")
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  NOTARY_ARGS=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD")
elif [ -n "${AC_API_KEY_PATH:-}" ] && [ -n "${AC_API_KEY_ID:-}" ] && [ -n "${AC_API_ISSUER_ID:-}" ]; then
  NOTARY_ARGS=(--key "$AC_API_KEY_PATH" --key-id "$AC_API_KEY_ID" --issuer "$AC_API_ISSUER_ID")
fi

if [ ${#NOTARY_ARGS[@]} -eq 0 ]; then
  warn "未提供公证凭据，已跳过公证（产物在 Gatekeeper 下仍需右键打开）"
  exit 0
fi

log "提交公证（可能需要数分钟）"
xcrun notarytool submit "$DMG_PATH" "${NOTARY_ARGS[@]}" --wait

log "staple 公证票据"
xcrun stapler staple "$DMG_PATH"
xcrun stapler validate "$DMG_PATH"

log "Gatekeeper 校验"
spctl --assess --type open --context context:primary-signature --verbose=4 "$DMG_PATH"

log "完成: $DMG_PATH"
