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

# 用项目中的 Info.plist 替换自动生成的（包含 LSUIElement=true）
cp Info.plist "$APP_PATH/Contents/Info.plist"

# ad-hoc 签名，让 macOS 能记住辅助功能权限
codesign -fs - "$APP_PATH"

echo "Built: $APP_PATH"
