// TranscriberStatus.qml
// Indicator widget for real-time transcriber status
import QtQuick
import Quickshell
import Quickshell.Io
import qs.services
import qs.modules.common
import qs.modules.common.widgets

MaterialSymbol {
    id: root

    text: micMuted ? "mic_off" : "mic"
    iconSize: Appearance.font.pixelSize.larger
    fill: serviceActive ? 1 : 0
    color: "white"

    // Property to track service status
    property bool serviceActive: false
    property bool micMuted: Audio.source?.audio?.muted ?? false
    property bool hovered: false

    // Connection health: "ok", "degraded", "error"
    property string connectionHealth: "ok"
    property bool healthBad: serviceActive && connectionHealth !== "ok"

    // Paths
    property string pidFile: Qt.resolvedUrl("file://" + transciberProjectPath + "/scripts/.transcribe.pid").toString().replace("file://", "")
    property string transciberProjectPath: "/home/agent/Work-Stuff/transcriber"
    property string toggleScript: transciberProjectPath + "/scripts/toggle_transcribe.sh"

    // Background pill
    Rectangle {
        anchors.fill: parent
        // Note: anchors.margins sets all four sides. Individual overrides (e.g.
        // anchors.leftMargin: -1) only take effect if they exceed this value,
        // since margins is the floor. Use per-side margins explicitly if needed.
        anchors.margins: -2
        z: -1
        radius: 4
        color: {
            if (!root.serviceActive) {
                return root.hovered ? "#9e9e9e" : "#757575"  // Gray (off)
            }
            if (root.connectionHealth === "error") {
                return root.hovered ? "#8b4049" : "#6e3038"  // Dusty red
            }
            if (root.connectionHealth === "degraded") {
                return root.hovered ? "#7d6a3a" : "#65542e"  // Dusty amber
            }
            return root.hovered ? "#3d7a4f" : "#316340"  // Dusty green
        }
        opacity: root.micMuted && root.serviceActive ? 0.62 : 1.0
    }

    // Click area to toggle the service
    MouseArea {
        anchors.fill: parent
        anchors.margins: -2
        onClicked: {
            toggleProcess.running = true
        }
        cursorShape: Qt.PointingHandCursor

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
            statusCheckProcess.running = true
        }

        onLoadFailed: error => {
            if (error == FileViewError.FileNotFound) {
                root.serviceActive = false
            }
        }
    }

    // Watch the health status file written by the Rust metrics task
    FileView {
        id: healthFileView
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/transcriber_health"
        watchChanges: true

        onFileChanged: this.reload()

        onLoaded: {
            root.connectionHealth = this.text().trim()
        }

        onLoadFailed: error => {
            root.connectionHealth = "ok"
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
