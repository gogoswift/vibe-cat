#!/bin/bash
set -e

TARGET="${1:-}"
if [ -n "$TARGET" ]; then
    cargo bundle --release --target "$TARGET"
    APP_PATH="target/$TARGET/release/bundle/osx/VibeCat.app"
else
    cargo bundle --release
    APP_PATH="target/release/bundle/osx/VibeCat.app"
fi

# 在 Info.plist 中添加 LSUIElement，防止 Dock 图标闪现
PLIST="$APP_PATH/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$PLIST" 2>/dev/null || \
/usr/libexec/PlistBuddy -c "Set :LSUIElement true" "$PLIST"

echo "Built: $APP_PATH (LSUIElement=true, no Dock icon)"
