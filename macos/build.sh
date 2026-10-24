#!/bin/bash
# Build macos/Transcriber.app (gitignored) for this checkout. Hotkeys and the bar open it with
#   open -n -g <repo>/macos/Transcriber.app
# Rebuilding changes the app's ad-hoc signature, so macOS may ask for the microphone again and
# the Accessibility entry may need re-adding.
set -euo pipefail
cd "$(dirname "$0")"
REPO="$(cd .. && pwd)"
APP=Transcriber.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp Info.plist "$APP/Contents/Info.plist"
clang -O2 -Wall -Wextra -DREPO="\"$REPO\"" -framework ApplicationServices -o "/Contents/MacOS/transcriber-launcher" launcher.c
codesign --force --sign - --identifier local.transcriber.launcher "$APP"
echo "built $PWD/$APP"
