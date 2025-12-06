#!/bin/bash
# Install the TranscriberStatus widget into the Quickshell bar
set -eu

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BAR_DIR="$HOME/.config/quickshell/ii/modules/ii/bar"
QML_SOURCE="$PROJECT_DIR/bar/TranscriberStatus.qml"
QML_DEST="$BAR_DIR/TranscriberStatus.qml"
BARCONTENT="$BAR_DIR/BarContent.qml"

echo "Installing TranscriberStatus widget..."

# Check if source exists
if [ ! -f "$QML_SOURCE" ]; then
    echo "ERROR: Source file not found: $QML_SOURCE"
    exit 1
fi

# Check if bar directory exists
if [ ! -d "$BAR_DIR" ]; then
    echo "ERROR: Bar directory not found: $BAR_DIR"
    echo "Is Quickshell ii config installed?"
    exit 1
fi

# Symlink the QML file
if [ -L "$QML_DEST" ]; then
    echo "Removing existing symlink..."
    rm "$QML_DEST"
elif [ -f "$QML_DEST" ]; then
    echo "Backing up existing file to $QML_DEST.bak"
    mv "$QML_DEST" "$QML_DEST.bak"
fi

ln -s "$QML_SOURCE" "$QML_DEST"
echo "Symlinked: $QML_SOURCE -> $QML_DEST"

# Check if already added to BarContent.qml
if grep -q "TranscriberStatus" "$BARCONTENT"; then
    echo "TranscriberStatus already present in BarContent.qml"
else
    echo ""
    echo "NOTE: You need to manually add the TranscriberStatus widget to BarContent.qml"
    echo ""
    echo "Add this inside the 'indicatorsRowLayout' RowLayout, before the NotificationUnreadCount Revealer:"
    echo ""
    echo '                    TranscriberStatus {'
    echo '                        Layout.alignment: Qt.AlignVCenter'
    echo '                        Layout.rightMargin: indicatorsRowLayout.realSpacing'
    echo '                    }'
    echo ""
    echo "File location: $BARCONTENT"
    echo "Look for 'Revealer { reveal: Notifications.silent' around line 298"
fi

echo ""
echo "Done! Restart Quickshell to see changes: qs reload"
