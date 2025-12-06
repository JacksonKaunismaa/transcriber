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

    // Three states: gray (off), green (running), orange (running but mic muted)
    property color activeColor: micMuted ? "#e65100" : "#2e7d32"  // Orange if muted, green if not
    property color activeHoverColor: micMuted ? "#ff6d00" : "#388e3c"
    property color inactiveColor: "#757575"
    property color inactiveHoverColor: "#9e9e9e"

    color: serviceActive ? activeColor : inactiveColor

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

        // Hover effect
        hoverEnabled: true
        onEntered: {
            root.color = serviceActive ? activeHoverColor : inactiveHoverColor
        }
        onExited: {
            root.color = serviceActive ? activeColor : inactiveColor
        }
    }

    // Process to check if transcriber is running
    // Checks if PID file exists and process is alive
    Process {
        id: statusCheckProcess
        command: ["bash", "-c", "test -f '" + pidFile + "' && kill -0 $(cat '" + pidFile + "') 2>/dev/null"]
        running: true

        onExited: (exitCode, exitStatus) => {
            // Exit code 0 means PID file exists and process is running
            serviceActive = (exitCode === 0)
        }
    }

    // Timer to regularly check status (every 3 seconds)
    Timer {
        interval: 3000
        running: true
        repeat: true
        onTriggered: {
            statusCheckProcess.running = true
        }
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
        onTriggered: statusCheckProcess.running = true
    }

    // Initial status check on load
    Component.onCompleted: {
        statusCheckProcess.running = true
    }
}
