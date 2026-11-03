import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Services.UPower
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Whitelist.js" as Whitelist

BarWidget {
    id: root
    moduleName: "raw.omapower"

    property bool setupReady: false
    property bool setupSupported: false
    property bool setupBusy: false
    property bool setupChecking: false
    property bool setupConfirming: false
    property var missingPackages: []
    property string setupError: ""
    readonly property string setupScript: decodeURIComponent(Qt.resolvedUrl("../setup.sh").toString().replace(/^file:\/\//, ""))
    property string binaryPath: (Quickshell.env("XDG_DATA_HOME") || (Quickshell.env("HOME") + "/.local/share")) + "/omapower/omapower"
    property string serviceState: "unknown"
    readonly property string powerProfile: {
        switch (PowerProfiles.profile) {
        case PowerProfile.Performance: return "performance"
        case PowerProfile.Balanced: return "balanced"
        case PowerProfile.PowerSaver: return "power-saver"
        default: return "unknown"
        }
    }
    property bool holding: false
    property bool suppressed: false
    property bool daemonAvailable: false
    property string detectionMode: ""
    property bool statusValid: false
    property var commands: []
    property string whitelistError: ""
    property string whitelistActionError: ""
    property bool whitelistBusy: false
    property string actionError: ""
    property bool actionBusy: false
    readonly property bool opened: panelLoader.item ? panelLoader.item.opened : false
    readonly property bool popoutSwitchClosing: panelLoader.item ? panelLoader.item.popoutSwitchClosing : false
    readonly property string description: (!setupReady ? strings.t("setup.required")
        : serviceState === "unknown" ? strings.t("status.unknown")
        : serviceState === "failed" ? strings.t("status.failed")
        : serviceState !== "active" ? strings.t("status.off")
        : !statusValid ? strings.t("status.starting")
        : !daemonAvailable ? strings.t("status.daemonMissing")
        : holding ? strings.t("status.holding")
        : suppressed ? strings.t("status.manual") : strings.t("status.idle"))
        + (serviceState === "active" && statusValid && detectionMode === "polling" ? " — " + strings.t("status.polling") : "")

    Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

    function applyWhitelist(state) {
        const result = state.configError ? null : Whitelist.parse(JSON.stringify(state))
        commands = result ? result.commands : []
        whitelistError = result ? "" : strings.t("whitelist.error")
    }
    function editWhitelist(action, name) {
        if (!setupReady || setupBusy || whitelistBusy || whitelistError) return false
        if (!Whitelist.validName(name)) { whitelistActionError = strings.t("whitelist.invalid"); return false }
        whitelistActionError = ""
        whitelistBusy = true
        whitelistAction.command = [root.binaryPath, action, name]
        whitelistAction.running = true
        return true
    }
    function open() {
        if (!panelLoader.item) return
        if (!setupReady && !setupBusy) checkSetup()
        if (setupReady && !serviceMonitor.running) { monitorRetry.stop(); serviceMonitor.running = true }
        statusFile.reload()
        panelLoader.item.open()
    }
    function close() { if (panelLoader.item) panelLoader.item.close() }
    function toggle() { if (opened) close(); else open() }
    function closeForPopoutSwitch() { if (panelLoader.item) panelLoader.item.closeForPopoutSwitch() }
    function injectPanel() {
        if (!panelLoader.item) return
        panelLoader.item.bar = root.bar
        panelLoader.item.anchorItem = button
        panelLoader.item.hostWidget = root
    }
    function toggleService() {
        if (!setupReady || setupBusy || actionBusy || serviceState === "unknown" || serviceState === "activating" || serviceState === "deactivating") return
        actionError = ""
        actionBusy = true
        serviceAction.command = ["systemctl", "--user", serviceState === "active" ? "stop" : "start", "omapower.service"]
        serviceAction.running = true
    }

    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight
    onBarChanged: injectPanel()
    onServiceStateChanged: {
        if (serviceState === "active") statusFile.reload()
        else statusValid = false
    }
    Component.onCompleted: checkSetup()

    function checkSetup() {
        if (setupCheck.running) return
        setupChecking = true
        setupCheck.running = true
    }
    function requestSetup() {
        if (setupBusy || setupChecking) return
        setupError = ""
        setupConfirming = true
    }
    function confirmSetup() {
        if (setupBusy || !setupSupported) return
        setupConfirming = false
        setupBusy = true
        setupError = ""
        setupAction.command = ["bash", setupScript, missingPackages.length ? "--install-deps" : "--setup"]
        setupAction.running = true
    }

    Process {
        id: setupCheck
        command: ["bash", root.setupScript, "--check"]
        stdout: StdioCollector { id: setupOutput }
        onExited: function(code) {
            root.setupChecking = false
            try {
                if (code !== 0) throw new Error("setup check failed")
                const state = JSON.parse(setupOutput.text)
                if (typeof state.ready !== "boolean" || typeof state.supported !== "boolean" || !Array.isArray(state.missing))
                    throw new Error("invalid setup state")
                root.setupReady = state.ready
                root.setupSupported = state.supported
                root.missingPackages = state.missing
                if (root.setupReady && !serviceMonitor.running) serviceMonitor.running = true
            } catch (e) { root.setupError = strings.t("setup.checkFailed") }
        }
    }
    Process {
        id: setupAction
        stderr: SplitParser { onRead: function(data) { console.warn("Omapower setup: " + data) } }
        onExited: function(code) {
            if (code === 0 && command[2] === "--install-deps") {
                command = ["bash", root.setupScript, "--setup"]
                running = true
                return
            }
            root.setupBusy = false
            if (code === 0) {
                // A rebuilt binary needs a fresh monitor as well as a restarted service.
                monitorRetry.stop()
                serviceMonitor.running = false
                root.checkSetup()
            } else {
                root.setupError = strings.t("setup.failed")
            }
        }
    }

    Loader {
        id: panelLoader
        active: true
        visible: false
        source: Qt.resolvedUrl("Panel.qml")
        onLoaded: { root.injectPanel(); Qt.callLater(root.injectPanel) }
        onStatusChanged: if (status === Loader.Error) console.error("Omapower: panel failed to load")
    }
    WidgetButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        // Font Awesome glyphs follow the bar foreground instead of emoji rendering.
        fontFamily: "Font Awesome 7 Free Solid"
        centerFigures: false
        text: root.powerProfile === "performance" ? "\uf0e7"
            : root.powerProfile === "balanced" ? "\uf6ad" : root.powerProfile === "power-saver" ? "\uf1b8" : "\uf059"
        tooltipText: root.description
        onPressed: function(buttonCode) { if (buttonCode === Qt.LeftButton) root.toggle() }
    }

    Process {
        id: serviceMonitor
        command: [root.binaryPath, "--watch-widget"]
        stdout: SplitParser {
            onRead: function(data) {
                try {
                    const state = JSON.parse(data)
                    if (typeof state.service === "string") root.serviceState = state.service
                    if (typeof state.configError === "boolean") root.applyWhitelist(state)
                } catch (e) { root.serviceState = "unknown" }
            }
        }
        onExited: { root.serviceState = "unknown"; if (root.setupReady && !root.setupBusy) monitorRetry.restart() }
    }
    // Recovery only: no timer runs while the D-Bus monitor is healthy.
    Timer { id: monitorRetry; interval: 5000; repeat: false; onTriggered: if (root.setupReady && !root.setupBusy) serviceMonitor.running = true }
    Process {
        id: serviceAction
        onExited: function(code) {
            root.actionBusy = false
            root.actionError = code === 0 ? "" : strings.t("action.failed")
        }
    }
    Process {
        id: whitelistAction
        onExited: function(code) {
            root.whitelistBusy = false
            root.whitelistActionError = code === 0 ? "" : strings.t("whitelist.saveFailed")
            if (panelLoader.item) panelLoader.item.whitelistEditFinished(code === 0)
        }
    }
    FileView {
        id: statusFile
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/omapower.json"
        printErrors: false
        watchChanges: true
        onFileChanged: reload()
        onLoaded: {
            try {
                const state = JSON.parse(text())
                if (typeof state.holding !== "boolean" || typeof state.suppressed !== "boolean") throw new Error("bad status")
                root.holding = state.holding
                root.suppressed = state.suppressed
                root.daemonAvailable = state.available !== false
                root.detectionMode = state.monitor || ""
                root.statusValid = true
            } catch (e) { root.statusValid = false }
        }
        onLoadFailed: root.statusValid = false
    }
}
