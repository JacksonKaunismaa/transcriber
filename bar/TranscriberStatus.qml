// TranscriberStatus.qml
// Indicator widget for real-time transcriber status
import QtQuick
import Quickshell
import Quickshell.Io
import qs.services

Rectangle {
    id: root

    // Widget dimensions and styling
    implicitWidth: statusText.width + 7
    implicitHeight: 15
    radius: 4

    // Property to track service status
    property bool serviceActive: false
    property bool micMuted: Audio.source?.audio?.muted ?? false
    property bool hovered: false  // Track hover state as property to preserve bindings

    // Compute color as a pure binding - never imperatively assign to `color`
    // This ensures the color always updates when serviceActive, micMuted, or hovered changes
    color: {
        if (!serviceActive) {
            return hovered ? "#9e9e9e" : "#757575"  // Gray (inactive)
        }
        if (micMuted) {
            return hovered ? "#ff6d00" : "#e65100"  // Orange (muted)
        }
        return hovered ? "#388e3c" : "#2e7d32"  // Green (active)
    }

    // Path to the PID file (relative to toggle script location)
    property string pidFile: Qt.resolvedUrl("file://" + transciberProjectPath + "/scripts/.transcribe.pid").toString().replace("file://", "")
    property string transciberProjectPath: "/home/agent/Work-Stuff/transcriber"
    property string toggleScript: transciberProjectPath + "/scripts/toggle_transcribe.sh"

    // Text display
    Text {
        id: statusText
        anchors.centerIn: parent
        text: "TR"
        color: "white"
        font.pixelSize: 10
        font.bold: true
    }

    // Click area to toggle the service
    MouseArea {
        anchors.fill: parent
        onClicked: {
            toggleProcess.running = true
        }
        cursorShape: Qt.PointingHandCursor

        // Hover effect - just toggle the property, let the binding handle the color
        hoverEnabled: true
        onEntered: root.hovered = true
        onExited: root.hovered = false
    }

    // Watch the PID file for instant detection of service start/stop
    FileView {
        id: pidFileView
        path: root.pidFile
        watchChanges: true

        onFileChanged: this.reload()

        onLoaded: {
            // PID file exists - verify the process is actually alive
            statusCheckProcess.running = true
        }

        onLoadFailed: error => {
            if (error == FileViewError.FileNotFound) {
                root.serviceActive = false
            }
        }
    }

    // Process to verify the PID from the file is a live process
    Process {
        id: statusCheckProcess
        command: ["bash", "-c", "kill -0 $(cat '" + pidFile + "') 2>/dev/null"]

        onExited: (exitCode, exitStatus) => {
            serviceActive = (exitCode === 0)
        }
    }

    // Safety-net timer: catches stale PID files (process crashed but file wasn't cleaned up)
    Timer {
        interval: 15000
        running: true
        repeat: true
        onTriggered: pidFileView.reload()
    }

    // Process to toggle the transcriber
    Process {
        id: toggleProcess
        command: ["bash", "-c", toggleScript]
        running: false

        onExited: (exitCode, exitStatus) => {
            // Check status after a short delay to let the process start/stop
            toggleTimer.start()
        }
    }

    // Timer to check status after toggle
    Timer {
        id: toggleTimer
        interval: 500
        repeat: false
        onTriggered: pidFileView.reload()
    }
}
