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
    echo "Adding TranscriberStatus to BarContent.qml..."

    # Find the line with Notifications.silent (unique anchor point)
    LINE=$(grep -n "Notifications.silent" "$BARCONTENT" | head -1 | cut -d: -f1)

    if [ -z "$LINE" ]; then
        echo "ERROR: Could not find 'Notifications.silent' in BarContent.qml"
        echo "File structure may have changed. Please add TranscriberStatus manually."
        exit 1
    fi

    # Verify the line before contains "Revealer {"
    PREV_LINE=$((LINE - 1))
    PREV_CONTENT=$(sed -n "${PREV_LINE}p" "$BARCONTENT")
    if ! echo "$PREV_CONTENT" | grep -q "Revealer {"; then
        echo "ERROR: Expected 'Revealer {' on line $PREV_LINE, but found:"
        echo "  $PREV_CONTENT"
        echo "File structure may have changed. Please add TranscriberStatus manually."
        exit 1
    fi

    # Backup before editing
    cp "$BARCONTENT" "$BARCONTENT.bak"
    echo "Backed up BarContent.qml to BarContent.qml.bak"

    # Insert before the Revealer block
    sed -i "${PREV_LINE}i\\
                    TranscriberStatus {\\
                        Layout.alignment: Qt.AlignVCenter\\
                        Layout.rightMargin: indicatorsRowLayout.realSpacing\\
                    }" "$BARCONTENT"

    # Verify the edit worked
    if grep -q "TranscriberStatus" "$BARCONTENT"; then
        echo "Added TranscriberStatus widget to BarContent.qml"
    else
        echo "ERROR: Edit failed. Restoring backup..."
        mv "$BARCONTENT.bak" "$BARCONTENT"
        exit 1
    fi
fi

echo ""
echo "Done! Restart Quickshell to see changes: qs reload"
