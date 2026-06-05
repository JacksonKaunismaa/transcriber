// VadDebugPanel.qml
// Live VAD debug overlay. Renders a sparkline of the last 10s of silero
// probabilities, the start/continue threshold lines, and current state.
// Visible only when the transcriber is running with `--debug-vad`.
//
// Wire it up by adding `VadDebugPanel { }` somewhere in your shell.qml (or
// equivalent root scope).
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

Scope {
    id: root

    property string snapshotPath: (Quickshell.env("XDG_RUNTIME_DIR") || "/tmp") + "/transcriber_vad.json"

    // Snapshot fields, updated each time the JSON file changes.
    property real currentProb: 0.0
    property bool inSpeech: false
    property int consecutiveSpeech: 0
    property int consecutiveSilence: 0
    property real startThreshold: 0.4
    property real continueThreshold: 0.15
    property int speechStartFrames: 3
    property int silenceStopFrames: 30
    property var history: []           // array of [ms_from_start, prob]
    property real writtenAtUnixMs: 0
    property bool snapshotFresh: false

    // Stale detector: snapshots older than 2s = transcriber stopped/crashed.
    // Also retries reload() when the snapshot is stale so the widget appears
    // when the user starts transcriber AFTER the shell — inotify can't watch
    // a file that doesn't exist yet.
    Timer {
        interval: 1000
        running: true
        repeat: true
        onTriggered: {
            const ageMs = Date.now() - root.writtenAtUnixMs
            const fresh = root.writtenAtUnixMs > 0 && ageMs < 2000
            root.snapshotFresh = fresh
            if (!fresh) snapshotView.reload()
        }
    }

    FileView {
        id: snapshotView
        path: root.snapshotPath
        watchChanges: true

        onFileChanged: this.reload()

        onLoaded: {
            try {
                const s = JSON.parse(this.text())
                root.currentProb = s.current_prob
                root.inSpeech = s.in_speech
                root.consecutiveSpeech = s.consecutive_speech
                root.consecutiveSilence = s.consecutive_silence
                root.startThreshold = s.speech_start_threshold
                root.continueThreshold = s.speech_continue_threshold
                root.speechStartFrames = s.speech_start_frames
                root.silenceStopFrames = s.silence_stop_frames
                root.history = s.history
                root.writtenAtUnixMs = s.written_at_unix_ms
                root.snapshotFresh = true
                sparkline.requestPaint()
            } catch (e) {
                // Mid-rename partial read — next reload event will retry.
            }
        }

        onLoadFailed: error => {
            root.snapshotFresh = false
        }
    }

    PanelWindow {
        id: panel
        visible: root.snapshotFresh
        color: "transparent"

        WlrLayershell.namespace: "transcriber:vad-debug"
        WlrLayershell.layer: WlrLayer.Overlay

        anchors {
            top: true
            right: true
        }
        margins {
            top: 60
            right: 16
        }

        exclusionMode: ExclusionMode.Ignore
        exclusiveZone: 0

        implicitWidth: 340
        implicitHeight: 200

        Rectangle {
            anchors.fill: parent
            radius: 8
            color: "#dd1a1a1f"
            border.width: 1
            border.color: "#3a3a45"

            Column {
                anchors.fill: parent
                anchors.margins: 10
                spacing: 6

                // ── Header row: state pill + current prob ────────────
                Row {
                    spacing: 10
                    width: parent.width

                    Rectangle {
                        width: 70
                        height: 22
                        radius: 4
                        color: root.inSpeech ? "#316340" : "#404048"
                        Text {
                            anchors.centerIn: parent
                            text: root.inSpeech ? "IN SPEECH" : "IDLE"
                            color: "white"
                            font.pixelSize: 11
                            font.bold: true
                            font.family: "monospace"
                        }
                    }
                    Text {
                        text: "p=" + root.currentProb.toFixed(3)
                        color: "white"
                        font.pixelSize: 14
                        font.family: "monospace"
                        anchors.verticalCenter: parent.verticalCenter
                    }
                    Text {
                        text: "spk:" + root.consecutiveSpeech + "/" + root.speechStartFrames
                              + "  sil:" + root.consecutiveSilence + "/" + root.silenceStopFrames
                        color: "#a0a0b0"
                        font.pixelSize: 11
                        font.family: "monospace"
                        anchors.verticalCenter: parent.verticalCenter
                    }
                }

                // ── Sparkline ───────────────────────────────────────
                Canvas {
                    id: sparkline
                    width: parent.width
                    height: parent.height - 60

                    onPaint: {
                        const ctx = getContext("2d")
                        ctx.reset()
                        const w = width
                        const h = height
                        const hist = root.history || []

                        // Background
                        ctx.fillStyle = "#101015"
                        ctx.fillRect(0, 0, w, h)

                        const probToY = p => h - (p * h) - 1

                        // Threshold lines
                        ctx.strokeStyle = "#5fdb6f"   // start (green)
                        ctx.setLineDash([4, 3])
                        ctx.lineWidth = 1
                        ctx.beginPath()
                        const yStart = probToY(root.startThreshold)
                        ctx.moveTo(0, yStart); ctx.lineTo(w, yStart); ctx.stroke()

                        ctx.strokeStyle = "#dba85f"   // continue (amber)
                        ctx.beginPath()
                        const yCont = probToY(root.continueThreshold)
                        ctx.moveTo(0, yCont); ctx.lineTo(w, yCont); ctx.stroke()

                        ctx.setLineDash([])

                        // Probability line
                        if (hist.length >= 2) {
                            const firstT = hist[0][0]
                            const lastT = hist[hist.length - 1][0]
                            const span = Math.max(1, lastT - firstT)
                            ctx.strokeStyle = "#9ec5ff"
                            ctx.lineWidth = 1.5
                            ctx.beginPath()
                            for (let i = 0; i < hist.length; i++) {
                                const t = hist[i][0]
                                const p = hist[i][1]
                                const x = ((t - firstT) / span) * w
                                const y = probToY(p)
                                if (i === 0) ctx.moveTo(x, y)
                                else ctx.lineTo(x, y)
                            }
                            ctx.stroke()
                        }

                        // Y-axis labels
                        ctx.fillStyle = "#606070"
                        ctx.font = "9px monospace"
                        ctx.fillText("1.0", 2, 10)
                        ctx.fillText(root.startThreshold.toFixed(1), 2, yStart - 2)
                        ctx.fillText(root.continueThreshold.toFixed(2), 2, yCont - 2)
                        ctx.fillText("0.0", 2, h - 2)
                    }

                    // Periodic repaint as a safety net for missed file events.
                    Timer {
                        interval: 250
                        running: panel.visible
                        repeat: true
                        onTriggered: sparkline.requestPaint()
                    }
                }
            }
        }
    }
}
