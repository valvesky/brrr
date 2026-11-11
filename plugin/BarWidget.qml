import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Services.UPower
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Whitelist.js" as Whitelist

// The plugin service (Service.qml) runs the helper and publishes its state to
// a runtime file; every widget instance reads that file. Edits go through the
// helper's command line, which updates the configuration the helper watches.
BarWidget {
    id: root
    moduleName: "raw.brrr"

    property string binaryPath: decodeURIComponent(String(Qt.resolvedUrl("../bin/brrr")).replace(/^file:\/\//, ""))
    property string statusPath: Quickshell.env("XDG_RUNTIME_DIR") + "/brrr-status.json"
    readonly property string powerProfile: {
        switch (PowerProfiles.profile) {
        case PowerProfile.Performance: return "performance"
        case PowerProfile.Balanced: return "balanced"
        case PowerProfile.PowerSaver: return "power-saver"
        default: return "unknown"
        }
    }
    property bool statusValid: false
    property bool running: false
    property string stopReason: "starting"
    property bool boostEnabled: false
    property bool configError: false
    property bool holding: false
    property bool suppressed: false
    property bool rejected: false
    property bool daemonAvailable: false
    property string detectionMode: ""
    property var commands: []
    property string whitelistError: ""
    property string whitelistActionError: ""
    property bool whitelistBusy: false
    property string actionError: ""
    property bool actionBusy: false
    readonly property bool canEdit: statusValid && running && !configError && !whitelistBusy && !actionBusy
    readonly property bool opened: panelLoader.item ? panelLoader.item.opened : false
    readonly property bool popoutSwitchClosing: panelLoader.item ? panelLoader.item.popoutSwitchClosing : false
    readonly property string description: (!statusValid ? strings.t("status.starting")
        : !running ? strings.t("status." + (["busy", "unsupported", "stopped", "legacy"].indexOf(stopReason) !== -1 ? stopReason : "starting"))
        : configError ? strings.t("status.configError")
        : !boostEnabled ? strings.t("status.off")
        : !daemonAvailable ? strings.t("status.daemonMissing")
        : holding ? strings.t("status.holding")
        : rejected ? strings.t("status.rejected")
        : suppressed ? strings.t("status.manual") : strings.t("status.idle"))
        + (statusValid && running && boostEnabled && detectionMode === "polling" ? " — " + strings.t("status.polling") : "")

    Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

    function applyStatus(raw) {
        try {
            const state = JSON.parse(raw)
            if (!state || typeof state.running !== "boolean") throw new Error("bad status")
            running = state.running
            if (!running) {
                stopReason = typeof state.reason === "string" ? state.reason : "stopped"
                statusValid = true
                return
            }
            if (typeof state.holding !== "boolean" || typeof state.enabled !== "boolean") throw new Error("bad status")
            const whitelist = Whitelist.parse(JSON.stringify({ commands: state.commands }))
            configError = state.configError === true
            boostEnabled = state.enabled
            holding = state.holding
            suppressed = state.suppressed === true
            rejected = state.rejected === true
            daemonAvailable = state.available === true
            detectionMode = typeof state.monitor === "string" ? state.monitor : ""
            commands = whitelist && !configError ? whitelist.commands : []
            whitelistError = configError || !whitelist ? strings.t("whitelist.error") : ""
            statusValid = true
        } catch (e) {
            statusValid = false
        }
    }
    function editWhitelist(action, name) {
        if (!canEdit) return false
        if (!Whitelist.validName(name)) { whitelistActionError = strings.t("whitelist.invalid"); return false }
        whitelistActionError = ""
        whitelistBusy = true
        whitelistAction.command = [root.binaryPath, action, name]
        whitelistAction.running = true
        return true
    }
    function toggleBoost() {
        if (!canEdit) return
        actionError = ""
        actionBusy = true
        boostAction.command = [root.binaryPath, boostEnabled ? "--disable" : "--enable"]
        boostAction.running = true
    }
    function open() {
        if (!panelLoader.item) return
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

    implicitWidth: button.implicitWidth
    implicitHeight: button.implicitHeight
    onBarChanged: injectPanel()

    Loader {
        id: panelLoader
        active: true
        visible: false
        source: Qt.resolvedUrl("Panel.qml")
        onLoaded: { root.injectPanel(); Qt.callLater(root.injectPanel) }
        onStatusChanged: if (status === Loader.Error) console.error("brrr: panel failed to load")
    }
    WidgetButton {
        id: button
        anchors.fill: parent
        bar: root.bar
        // Icons come from the bar's own Nerd Font, like Omarchy's built-in
        // widgets: bolt, yin-yang, recycle, or a question mark.
        centerFigures: false
        text: root.powerProfile === "performance" ? "\uf0e7"
            : root.powerProfile === "balanced" ? "\udb81\ude80"
            : root.powerProfile === "power-saver" ? "\uf1b8" : "\uf059"
        tooltipText: root.description
        onPressed: function(buttonCode) { if (buttonCode === Qt.LeftButton) root.toggle() }
    }

    Process {
        id: boostAction
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
        path: root.statusPath
        printErrors: false
        watchChanges: true
        onFileChanged: reload()
        onLoaded: root.applyStatus(text())
        onLoadFailed: { root.statusValid = false; root.stopReason = "starting" }
    }
}
