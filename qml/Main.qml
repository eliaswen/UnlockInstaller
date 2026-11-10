// Main window. All state comes from the Backend object (src/ui.rs) as JSON:
// `v` (Backend.view) holds everything but the game and DLC lists.

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import UnlockInstaller

ApplicationWindow {
    id: root
    width: 1100
    height: 720
    visible: true
    title: v.appName

    readonly property var v: JSON.parse(Backend.view)
    readonly property var gameList: JSON.parse(Backend.games)
    readonly property var dlcList: JSON.parse(Backend.dlcs)
    // Kept separately so the combo box is only reset when the list changes.
    property var proxyList: []
    onVChanged: {
        if (JSON.stringify(v.proxies) !== JSON.stringify(proxyList))
            proxyList = v.proxies
    }

    readonly property color successColor: "#26a269"
    readonly property color errorColor: "#c01c28"
    readonly property color warningColor: "#c64600"

    function noteColor(cls) {
        return cls === "success" ? successColor : cls === "warning" ? warningColor : palette.windowText
    }

    // Under Wine the system fonts are often missing or incomplete.
    FontLoader { id: notoRegular; source: "qrc:/assets/fonts/NotoSans-Regular.ttf" }
    FontLoader { id: notoBold; source: "qrc:/assets/fonts/NotoSans-Bold.ttf" }
    Binding {
        target: root
        property: "font.family"
        value: notoRegular.name
        when: root.v.wine && notoRegular.status === FontLoader.Ready
    }

    Connections {
        target: Backend
        function onDlcDownloadsRequired(names) {
            dlcDialog.names = names
            dlcDialog.open()
        }
        function onPasswordNeeded(title, wrong) {
            passwordWindow.ask(title, wrong)
        }
    }

    Component.onCompleted: {
        proxyList = v.proxies
        if (v.wine)
            wineDialog.open()
        Backend.start()
    }

    // A dimmed label.
    component DimLabel: Label {
        opacity: 0.7
    }
    // Read-only text that can be selected and copied.
    component SelectableText: TextEdit {
        readOnly: true
        selectByMouse: true
        wrapMode: TextEdit.Wrap
        color: sys.windowText
        selectionColor: sys.highlight
        selectedTextColor: sys.highlightedText
        SystemPalette { id: sys }
    }
    component Banner: Rectangle {
        default property alias content: row.data
        Layout.fillWidth: true
        implicitHeight: row.implicitHeight + 16
        color: Qt.rgba(sys.highlight.r, sys.highlight.g, sys.highlight.b, 0.15)
        SystemPalette { id: sys }
        RowLayout {
            id: row
            anchors.fill: parent
            anchors.leftMargin: 12
            anchors.rightMargin: 12
            spacing: 12
        }
    }

    header: ToolBar {
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 6
            anchors.rightMargin: 6
            ToolButton {
                text: root.v.rescanLabel
                enabled: root.v.rescanEnabled
                onClicked: Backend.rescan()
                ToolTip.visible: hovered
                ToolTip.text: "Scan the Steam libraries again"
            }
            Item { Layout.fillWidth: true }
            ToolButton {
                id: menuButton
                text: "Menu"
                onClicked: mainMenu.popup(menuButton, 0, menuButton.height)
                Menu {
                    id: mainMenu
                    MenuItem { text: "Settings"; onTriggered: settingsWindow.open() }
                    MenuItem {
                        text: "Uninstall from all games…"
                        enabled: root.v.uninstallAllEnabled
                        onTriggered: uninstallAllDialog.open()
                    }
                    MenuItem { text: "Check for updates"; onTriggered: Backend.checkUpdates() }
                    MenuItem { text: "About " + root.v.appName; onTriggered: aboutDialog.open() }
                }
            }
        }
    }

    footer: Pane {
        padding: 0
        topPadding: 5
        bottomPadding: 6
        leftPadding: 12
        rightPadding: 12
        DimLabel {
            width: parent.width
            text: root.v.status
            elide: Text.ElideRight
            HoverHandler { id: statusHover }
            ToolTip.visible: statusHover.hovered && truncated
            ToolTip.text: text
        }
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Banner {
            visible: root.v.update !== ""
            Label { text: root.v.update; wrapMode: Text.Wrap; Layout.fillWidth: true }
            Button { text: "View release"; onClicked: Qt.openUrlExternally(root.v.releaseUrl) }
            Button { text: "Dismiss"; onClicked: Backend.dismissUpdate() }
        }
        Banner {
            visible: root.v.noImports
            Label {
                text: "No unlocker has been imported yet. Download or import one to get started."
                wrapMode: Text.Wrap
                Layout.fillWidth: true
            }
            Button { text: "Open Settings"; onClicked: settingsWindow.open() }
        }

        SplitView {
            Layout.fillWidth: true
            Layout.fillHeight: true

            // --- game list (left) ---------------------------------------------
            ColumnLayout {
                SplitView.preferredWidth: 340
                SplitView.minimumWidth: 220
                spacing: 6
                TextField {
                    Layout.fillWidth: true
                    Layout.margins: 8
                    Layout.bottomMargin: 0
                    placeholderText: "Search games…"
                    onTextChanged: Backend.setSearch(text)
                }
                ListView {
                    id: gamesView
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: root.gameList
                    ScrollBar.vertical: ScrollBar {}
                    delegate: ItemDelegate {
                        id: gameRow
                        required property var modelData
                        width: ListView.view.width - 12
                        x: 6
                        highlighted: modelData.appid === root.v.appid
                        onClicked: Backend.select(modelData.appid)
                        contentItem: RowLayout {
                            spacing: 8
                            GameIcon { appid: gameRow.modelData.appid; name: gameRow.modelData.name; size: 40 }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                Label {
                                    Layout.fillWidth: true
                                    text: gameRow.modelData.name
                                    elide: Text.ElideRight
                                }
                                Label {
                                    Layout.fillWidth: true
                                    text: gameRow.modelData.subtitle
                                    elide: Text.ElideRight
                                    font.pointSize: root.font.pointSize * 0.85
                                    color: gameRow.highlighted ? palette.highlightedText
                                         : gameRow.modelData.installed ? root.successColor : palette.windowText
                                    opacity: gameRow.modelData.installed || gameRow.highlighted ? 1 : 0.7
                                }
                            }
                        }
                        ToolTip.visible: hovered
                        ToolTip.delay: 800
                        ToolTip.text: modelData.name
                    }
                    DimLabel {
                        anchors.horizontalCenter: parent.horizontalCenter
                        y: 24
                        visible: gamesView.count === 0
                        text: root.v.noGamesText
                    }
                }
            }

            // --- details (right) ----------------------------------------------
            StackLayout {
                SplitView.fillWidth: true
                SplitView.minimumWidth: 420
                currentIndex: root.v.hasSelection ? 1 : 0

                DimLabel {
                    text: "Select a game to get started"
                    font.pointSize: root.font.pointSize * 1.3
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                }

                Item {
                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: 18
                        anchors.bottomMargin: 12
                        spacing: 12

                        // summary
                        RowLayout {
                            spacing: 12
                            GameIcon {
                                Layout.alignment: Qt.AlignTop
                                appid: root.v.appid
                                name: root.v.title || ""
                                size: 64
                            }
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 4
                                Label {
                                    Layout.fillWidth: true
                                    text: root.v.title || ""
                                    font.pointSize: root.font.pointSize * 1.5
                                    font.bold: true
                                    wrapMode: Text.Wrap
                                }
                                DimLabel { text: root.v.info || "" }
                                SelectableText {
                                    Layout.fillWidth: true
                                    text: root.v.path || ""
                                    opacity: 0.7
                                    font.family: root.font.family
                                    font.pointSize: root.font.pointSize * 0.85
                                }
                                SelectableText {
                                    Layout.fillWidth: true
                                    text: root.v.apis || ""
                                    opacity: 0.7
                                    font.family: root.font.family
                                    font.pointSize: root.font.pointSize * 0.85
                                }
                            }
                        }

                        // options
                        GridLayout {
                            columns: 3
                            rowSpacing: 8
                            columnSpacing: 12
                            DimLabel { text: "Unlocker"; Layout.alignment: Qt.AlignRight }
                            ComboBox {
                                id: unlockerBox
                                model: ["SmokeAPI", "CreamAPI"]
                                currentIndex: root.v.unlocker
                                onActivated: index => {
                                    Backend.setUnlocker(index)
                                    currentIndex = Qt.binding(() => root.v.unlocker)
                                }
                            }
                            Item { Layout.fillWidth: true }

                            DimLabel { text: "Method"; Layout.alignment: Qt.AlignRight }
                            ComboBox {
                                id: methodBox
                                implicitContentWidthPolicy: ComboBox.WidestText
                                model: ["Direct (replace steam_api)", "Koaloader proxy"]
                                enabled: root.v.methodEnabled
                                currentIndex: root.v.method
                                onActivated: index => {
                                    Backend.setMethod(index)
                                    currentIndex = Qt.binding(() => root.v.method)
                                }
                            }
                            Item {}

                            DimLabel {
                                text: "Proxy DLL"
                                visible: root.v.koaloader
                                Layout.alignment: Qt.AlignRight
                            }
                            ComboBox {
                                id: proxyBox
                                visible: root.v.koaloader
                                model: root.proxyList
                                currentIndex: root.v.proxy
                                onActivated: index => {
                                    Backend.setProxy(index)
                                    currentIndex = Qt.binding(() => root.v.proxy)
                                }
                            }
                            Label {
                                visible: root.v.koaloader
                                Layout.fillWidth: true
                                text: root.v.proxyNote || ""
                                elide: Text.ElideRight
                                font.pointSize: root.font.pointSize * 0.85
                                color: root.noteColor(root.v.proxyNoteClass)
                                opacity: root.v.proxyNoteClass === "dim" ? 0.7 : 1
                                HoverHandler { id: noteHover }
                                ToolTip.visible: noteHover.hovered
                                ToolTip.text: root.v.proxyNoteTip || ""
                            }
                        }

                        SelectableText {
                            Layout.fillWidth: true
                            visible: root.v.koaloader
                            text: root.v.hint || ""
                            opacity: 0.7
                            font.family: root.font.family
                            font.pointSize: root.font.pointSize * 0.85
                        }

                        RowLayout {
                            visible: root.v.launchVisible || false
                            spacing: 6
                            Label { text: "Steam launch options" }
                            TextField {
                                id: launchField
                                Layout.fillWidth: true
                                readOnly: true
                                text: root.v.launchText || ""
                            }
                            Button {
                                text: "Copy"
                                onClicked: {
                                    launchField.selectAll()
                                    launchField.copy()
                                    launchField.deselect()
                                    Backend.setStatus("Launch options copied to clipboard")
                                }
                            }
                        }

                        // install / uninstall
                        RowLayout {
                            spacing: 8
                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2
                                SelectableText {
                                    Layout.fillWidth: true
                                    text: root.v.installed || ""
                                    font: root.font
                                    color: root.v.installedOk ? root.successColor : palette.windowText
                                }
                                Label {
                                    Layout.fillWidth: true
                                    visible: root.v.installBlocker !== ""
                                    text: root.v.installBlocker
                                    wrapMode: Text.Wrap
                                    color: root.warningColor
                                    font.pointSize: root.font.pointSize * 0.85
                                }
                            }
                            Button {
                                text: "Uninstall"
                                enabled: root.v.uninstallEnabled
                                onClicked: Backend.uninstall()
                            }
                            Button {
                                text: root.v.installLabel
                                highlighted: true
                                enabled: root.v.installEnabled
                                onClicked: Backend.install()
                            }
                        }

                        // DLCs
                        RowLayout {
                            Layout.topMargin: 6
                            spacing: 6
                            readonly property var dlc: root.v.dlc || {}
                            Label {
                                Layout.fillWidth: true
                                text: parent.dlc.text || ""
                                font.bold: true
                                elide: Text.ElideRight
                            }
                            BusyIndicator {
                                visible: parent.dlc.spinning || false
                                running: visible
                                Layout.preferredWidth: 24
                                Layout.preferredHeight: 24
                            }
                            Button { text: "All"; visible: parent.dlc.loaded || false; onClicked: Backend.setAllDlcs(true) }
                            Button { text: "None"; visible: parent.dlc.loaded || false; onClicked: Backend.setAllDlcs(false) }
                            Button {
                                text: parent.dlc.refreshLabel || "Refresh"
                                visible: parent.dlc.refreshVisible || false
                                onClicked: Backend.reloadDlcs()
                            }
                            Button {
                                text: parent.dlc.checkLabel || ""
                                visible: parent.dlc.loaded || false
                                enabled: parent.dlc.checkEnabled || false
                                onClicked: Backend.checkDlcDownloads()
                            }
                        }
                        Frame {
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            padding: 1
                            ListView {
                                anchors.fill: parent
                                clip: true
                                model: root.dlcList
                                ScrollBar.vertical: ScrollBar {}
                                delegate: RowLayout {
                                    id: dlcRow
                                    required property var modelData
                                    width: ListView.view.width - 16
                                    x: 6
                                    spacing: 8
                                    CheckBox {
                                        Layout.fillWidth: true
                                        text: dlcRow.modelData.name
                                        checked: dlcRow.modelData.enabled
                                        onToggled: Backend.toggleDlc(dlcRow.modelData.id, checked)
                                    }
                                    DimLabel {
                                        text: dlcRow.modelData.id
                                        font.pointSize: root.font.pointSize * 0.85
                                    }
                                    Button {
                                        text: "View"
                                        flat: true
                                        onClicked: Qt.openUrlExternally(
                                            "https://store.steampowered.com/app/" + dlcRow.modelData.id + "/?l=english")
                                        ToolTip.visible: hovered
                                        ToolTip.text: "Open this DLC's Steam store page"
                                    }
                                    DimLabel {
                                        visible: text !== ""
                                        text: dlcRow.modelData.status
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // --- dialogs -------------------------------------------------------------

    Dialog {
        id: dlcDialog
        property string names
        property int choice: 0
        anchors.centerIn: Overlay.overlay
        width: Math.min(root.width - 80, 520)
        modal: true
        title: "Some enabled DLCs require a download"
        onAboutToShow: choice = 0
        onClosed: Backend.answerDlcDownloads(choice)
        Label {
            width: parent.width
            wrapMode: Text.Wrap
            text: "Steam may need to download files for:\n\n" + dlcDialog.names
                  + "\n\nYou can continue with these DLCs enabled, or disable them before installing."
        }
        footer: DialogButtonBox {
            Button { text: "Cancel"; onClicked: { dlcDialog.choice = 0; dlcDialog.close() } }
            Button { text: "Continue anyway"; onClicked: { dlcDialog.choice = 1; dlcDialog.close() } }
            Button { text: "Don't enable those DLCs"; onClicked: { dlcDialog.choice = 2; dlcDialog.close() } }
        }
    }

    Dialog {
        id: uninstallAllDialog
        anchors.centerIn: Overlay.overlay
        width: Math.min(root.width - 80, 460)
        modal: true
        title: "Uninstall from all games?"
        Label {
            width: parent.width
            wrapMode: Text.Wrap
            text: "Every detected unlocker will be removed and the original files restored."
        }
        footer: DialogButtonBox {
            Button { text: "Cancel"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
            Button { text: "Uninstall all"; DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole }
        }
        onAccepted: Backend.uninstallAll()
    }

    Dialog {
        id: wineDialog
        anchors.centerIn: Overlay.overlay
        width: Math.min(root.width - 80, 520)
        modal: true
        title: "Running under Wine or Proton"
        Label {
            width: parent.width
            wrapMode: Text.Wrap
            text: "This is the Windows version of " + root.v.appName + ", and it is running under Wine "
                  + "or Proton. It cannot see your Steam libraries properly from here.\n\n"
                  + "Run the native Linux version instead: it handles Proton games directly."
        }
        footer: DialogButtonBox {
            Button { text: "Quit"; DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole }
            Button { text: "Continue anyway"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
        }
        onAccepted: Qt.quit()
    }

    Dialog {
        id: aboutDialog
        anchors.centerIn: Overlay.overlay
        modal: true
        title: "About " + root.v.appName
        standardButtons: Dialog.Close
        ColumnLayout {
            spacing: 8
            Label { text: root.v.appName + " " + root.v.version; font.bold: true }
            Label { text: "Installs SmokeAPI and CreamAPI DLC unlockers into Steam games." }
        }
    }

    SettingsWindow {
        id: settingsWindow
        transientParent: root
        v: root.v
        font: root.font
    }

    // Asks for an archive's password; a window of its own so that it can sit
    // above the settings window.
    ApplicationWindow {
        id: passwordWindow
        transientParent: settingsWindow.visible ? settingsWindow : root
        modality: Qt.ApplicationModal
        flags: Qt.Dialog
        width: 380
        height: passwordColumn.implicitHeight + 24
        font: root.font
        property bool answered: false

        function ask(title, wrong) {
            passwordWindow.title = title
            passwordPrompt.text = wrong ? "Wrong password, try again:"
                                        : "This archive is password-protected. Enter its password:"
            passwordField.text = ""
            answered = false
            show()
            requestActivate()
            passwordField.forceActiveFocus()
        }
        function submit() {
            answered = true
            close()
            Backend.submitPassword(passwordField.text)
        }
        onClosing: if (!answered) Backend.cancelPassword()

        ColumnLayout {
            id: passwordColumn
            anchors.fill: parent
            anchors.margins: 12
            spacing: 10
            Label { id: passwordPrompt; Layout.fillWidth: true; wrapMode: Text.Wrap }
            TextField {
                id: passwordField
                Layout.fillWidth: true
                echoMode: TextInput.Password
                onAccepted: passwordWindow.submit()
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button { text: "Cancel"; onClicked: passwordWindow.close() }
                Button { text: "Import"; highlighted: true; onClicked: passwordWindow.submit() }
            }
        }
    }
}
