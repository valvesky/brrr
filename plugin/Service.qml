import QtQuick
import Quickshell
import Quickshell.Io

// Runs one brrr helper for as long as the shell runs, whatever the number
// of bars, and publishes its state to a runtime file for the bar widgets.
// Replacement bars do not hand widgets live service objects, so the file is
// the one interface every bar can read.
//
// Lifetime: the helper exits when its stdin pipe closes, so it never outlives
// the shell. Plugin updates reload this service, which restarts the helper
// from the updated checkout.
Scope {
    id: root
    property var shell: null
    property var manifest: null

    readonly property string binDir: decodeURIComponent(String(Qt.resolvedUrl("../bin")).replace(/^file:\/\//, ""))
    readonly property string statusPath: Quickshell.env("XDG_RUNTIME_DIR") + "/brrr-status.json"
    readonly property int exitUnsupported: 69
    readonly property int exitBusy: 75
    property bool stopping: false
    property int retireExit: -1
    property int helperExit: -1

    // Writes block so they land in order; a service being destroyed writes
    // nothing, so it can never overwrite its replacement's state.
    function publish(state) { if (!stopping) statusFile.setText(JSON.stringify(state) + "\n") }

    Component.onCompleted: {
        publish({ running: false, reason: "starting" })
        retire.running = true
    }
    Component.onDestruction: stopping = true

    // Quickshell reports a failed start only through `running`, never
    // `exited`, so both processes recover from runningChanged. The exit code
    // (if any) is read after both signals have been delivered.
    function retireFinished() {
        if (stopping) return
        if (retireExit === 0) {
            helper.running = true
        } else {
            // The old daemon has no lock: never run beside it.
            publish({ running: false, reason: "legacy" })
            retireRetry.restart()
        }
        retireExit = -1
    }
    function helperFinished() {
        if (stopping) return
        const reason = helperExit === exitBusy ? "busy" : helperExit === exitUnsupported ? "unsupported" : "stopped"
        publish({ running: false, reason: reason })
        if (reason !== "unsupported") helperRetry.restart()
        helperExit = -1
    }

    Process {
        id: retire
        command: [root.binDir + "/retire-legacy-service"]
        stderr: SplitParser { onRead: function(data) { console.warn("brrr: " + data) } }
        onExited: function(code) { root.retireExit = code }
        onRunningChanged: if (!running) Qt.callLater(root.retireFinished)
    }

    Process {
        id: helper
        command: [root.binDir + "/brrr", "--supervised"]
        stdinEnabled: true
        stdout: SplitParser {
            onRead: function(data) {
                try {
                    const state = JSON.parse(data)
                    if (!state || typeof state.holding !== "boolean" || !Array.isArray(state.commands))
                        throw new Error("invalid state")
                    state.running = true
                    root.publish(state)
                } catch (e) {
                    console.warn("brrr: ignoring invalid helper output")
                }
            }
        }
        stderr: SplitParser { onRead: function(data) { console.log("brrr: " + data) } }
        onExited: function(code) { root.helperExit = code }
        onRunningChanged: if (!running) Qt.callLater(root.helperFinished)
    }

    // Recovery only: nothing polls while the helper is running.
    Timer { id: helperRetry; interval: 5000; onTriggered: helper.running = true }
    Timer { id: retireRetry; interval: 5000; onTriggered: retire.running = true }

    FileView {
        id: statusFile
        path: root.statusPath
        atomicWrites: true
        blockWrites: true
        printErrors: false
    }
}
