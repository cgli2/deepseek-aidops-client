#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_NAME="AIOPS Desktop"
BUNDLE_ID="com.clotee.aidops"
TEAM_ID="VATCH8RNM8"
DEVELOPMENT_IDENTITY="Apple Development: shida Diao (7C24A7H96Y)"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -n 1)"
BUILD_NUMBER="${MACOS_BUILD_NUMBER:-${VERSION//./}}"
APP_DIR="$ROOT_DIR/dist/$APP_NAME.app"
DMG_PATH="$ROOT_DIR/dist/AIOPS-Desktop-$VERSION.dmg"
ICON_SOURCE="$ROOT_DIR/bin/assets/icon_1024.png"
ICNS_PATH="$ROOT_DIR/bin/assets/AppIcon.icns"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "[macos] packaging must run on macOS" >&2
  exit 1
fi

identities="$(security find-identity -v -p codesigning 2>/dev/null || true)"
has_identity() {
  [[ "$identities" == *\"$1\"* ]]
}

find_developer_id() {
  printf '%s\n' "$identities" \
    | sed -n 's/^[[:space:]]*[0-9]*) [0-9A-F]* "\(Developer ID Application:.*(VATCH8RNM8)\)"$/\1/p' \
    | head -n 1
}

signing_mode="${MACOS_SIGNING_MODE:-auto}"
signing_identity="${MACOS_SIGNING_IDENTITY:-}"
if [[ -n "$signing_identity" ]]; then
  if ! has_identity "$signing_identity"; then
    echo "[macos] signing identity not found: $signing_identity" >&2
    exit 1
  fi
elif [[ "$signing_mode" == "adhoc" ]]; then
  signing_identity="-"
elif [[ "$signing_mode" == "development" ]]; then
  if ! has_identity "$DEVELOPMENT_IDENTITY"; then
    echo "[macos] development identity not found: $DEVELOPMENT_IDENTITY" >&2
    exit 1
  fi
  signing_identity="$DEVELOPMENT_IDENTITY"
elif [[ "$signing_mode" == "release" ]]; then
  signing_identity="$(find_developer_id)"
  if [[ -z "$signing_identity" ]]; then
    echo "[macos] release requires: Developer ID Application: ... ($TEAM_ID)" >&2
    echo "[macos] Apple Distribution certificates are only for App Store distribution." >&2
    exit 1
  fi
elif [[ "$signing_mode" == "auto" ]]; then
  signing_identity="$(find_developer_id)"
  if [[ -z "$signing_identity" ]] && has_identity "$DEVELOPMENT_IDENTITY"; then
    signing_identity="$DEVELOPMENT_IDENTITY"
    signing_mode="development"
  elif [[ -z "$signing_identity" ]]; then
    signing_identity="-"
    signing_mode="adhoc"
  else
    signing_mode="release"
  fi
else
  echo "[macos] invalid MACOS_SIGNING_MODE: $signing_mode (auto|development|release|adhoc)" >&2
  exit 1
fi

if [[ "$signing_identity" == Apple\ Distribution:* ]]; then
  echo "[macos] Apple Distribution is not supported by the DMG release workflow." >&2
  exit 1
fi

if [[ -n "${MACOS_NOTARY_PROFILE:-}" && "$signing_identity" != Developer\ ID\ Application:* ]]; then
  echo "[macos] notarization requires a Developer ID Application identity" >&2
  exit 1
fi

echo "[macos] signing mode: $signing_mode"
echo "[macos] signing identity: $signing_identity"

cd "$ROOT_DIR"
mkdir -p dist target/macos

if [[ ! -f "$ICON_SOURCE" || ! -f "$ICNS_PATH" ]]; then
  python3 scripts/make_icon.py --mac-only
fi

if [[ "${MACOS_UNIVERSAL:-0}" == "1" ]]; then
  archs=(aarch64-apple-darwin x86_64-apple-darwin)
  for target in "${archs[@]}"; do
    rustup target add "$target"
    cargo build --release --all-features --target "$target"
  done
  lipo -create \
    target/aarch64-apple-darwin/release/aidops-desktop \
    target/x86_64-apple-darwin/release/aidops-desktop \
    -output target/macos/aidops-desktop
else
  target="$(rustc -vV | sed -n 's/^host: //p')"
  cargo build --release --all-features --target "$target"
  cp "target/$target/release/aidops-desktop" target/macos/aidops-desktop
fi

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources/config" "$APP_DIR/Contents/Resources/docs"
cp target/macos/aidops-desktop "$APP_DIR/Contents/MacOS/aidops-desktop"
chmod 755 "$APP_DIR/Contents/MacOS/aidops-desktop"
cp "$ICNS_PATH" "$APP_DIR/Contents/Resources/AppIcon.icns"
cp config/default.toml "$APP_DIR/Contents/Resources/config/default.toml"
cp extensions/EXTENSION-COOKBOOK.md "$APP_DIR/Contents/Resources/docs/EXTENSION-COOKBOOK.md"
sed -e "s/@BUNDLE_ID@/$BUNDLE_ID/g" \
    -e "s/@VERSION@/$VERSION/g" \
    -e "s/@BUILD_NUMBER@/$BUILD_NUMBER/g" \
    macos/Info.plist.in > "$APP_DIR/Contents/Info.plist"

sign_args=(--force --deep --sign "$signing_identity")
if [[ "$signing_identity" != "-" ]]; then
  sign_args+=(--options runtime --timestamp --entitlements macos/entitlements.plist)
fi
codesign "${sign_args[@]}" "$APP_DIR"
codesign --verify --deep --strict --verbose=2 "$APP_DIR"
plutil -lint "$APP_DIR/Contents/Info.plist"

staging_dir="$(mktemp -d "${TMPDIR:-/tmp}/aidops-dmg.XXXXXX")"
trap 'rm -rf "$staging_dir"' EXIT
cp -R "$APP_DIR" "$staging_dir/"
ln -s /Applications "$staging_dir/Applications"
rm -f "$DMG_PATH"
hdiutil create -volname "$APP_NAME" -srcfolder "$staging_dir" -ov -format UDZO "$DMG_PATH" >/dev/null

if [[ "$signing_identity" != "-" ]]; then
  codesign --force --sign "$signing_identity" --timestamp "$DMG_PATH"
  codesign --verify --verbose=2 "$DMG_PATH"
fi

if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
  xcrun notarytool submit "$DMG_PATH" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG_PATH"
  xcrun stapler validate "$DMG_PATH"
fi

echo "[macos] app: $APP_DIR"
echo "[macos] dmg: $DMG_PATH"
echo "[macos] bundle id: $BUNDLE_ID"
echo "[macos] team id: $TEAM_ID"
