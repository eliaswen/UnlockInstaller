// Settings window: unlocker imports, defaults and folders.
// `v` is the main window's parsed Backend.view; its `settings` member holds
// what is shown here.

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import UnlockInstaller

ApplicationWindow {
    id: win
    property var v
    readonly property var s: v.settings

    title: "Settings"
    width: 600
    height: Math.min(body.implicitHeight + 36, 760)
    modality: Qt.WindowModal
    flags: Qt.Dialog

    function open() {
        show()
        raise()
        requestActivate()
    }

    component DimLabel: Label {
        opacity: 0.7
    }
    component Caption: Label {
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        opacity: 0.7
        font.pointSize: win.font.pointSize * 0.85
    }
    component SectionTitle: Label {
        font.bold: true
        Layout.topMargin: 12
    }
    // A framed list of setting rows.
    component Section: Frame {
        default property alias rows: column.data
        Layout.fillWidth: true
        ColumnLayout {
            id: column
            anchors.fill: parent
            spacing: 10
        }
    }

    FileDialog {
        id: archiveDialog
        property int kind
        property string kindName
        parentWindow: win
        title: "Select the " + kindName + " release archive"
        nameFilters: win.s.archiveFilters
        onAccepted: Backend.importArchive(kind, selectedFile.toString())
    }
    FolderDialog {
        id: folderDialog
        property int folder
        property string folderTitle
        parentWindow: win
        title: "Select the " + folderTitle.toLowerCase() + " folder"
        onAccepted: Backend.setFolder(folder, selectedFolder.toString())
    }

    ScrollView {
        anchors.fill: parent
        contentWidth: availableWidth

        ColumnLayout {
            id: body
            width: parent.width
            spacing: 8

            Item { implicitHeight: 6 }

            SectionTitle { text: "Unlockers"; Layout.topMargin: 0; Layout.leftMargin: 18 }
            Section {
                Layout.leftMargin: 18
                Layout.rightMargin: 18
                Repeater {
                    model: win.s.imports
                    RowLayout {
                        id: importRow
                        required property var modelData
                        Layout.fillWidth: true
                        spacing: 8
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label { text: importRow.modelData.name }
                            Caption { text: importRow.modelData.status }
                        }
                        Button {
                            visible: importRow.modelData.downloadTip !== ""
                            enabled: !importRow.modelData.downloading
                            text: "Download"
                            onClicked: Backend.download(importRow.modelData.index)
                            ToolTip.visible: hovered
                            ToolTip.text: importRow.modelData.downloadTip
                        }
                        Button {
                            text: "Import…"
                            onClicked: {
                                archiveDialog.kind = importRow.modelData.index
                                archiveDialog.kindName = importRow.modelData.name
                                archiveDialog.open()
                            }
                            ToolTip.visible: hovered
                            ToolTip.text: importRow.modelData.importTip
                        }
                    }
                }
            }
            Caption {
                Layout.leftMargin: 18
                Layout.rightMargin: 18
                text: "No unlocker files ship with the app. Download the latest official release, or "
                      + "import an archive you downloaded yourself. Koaloader is only needed for the "
                      + "Koaloader install method."
            }

            SectionTitle { text: "General"; Layout.leftMargin: 18 }
            Section {
                Layout.leftMargin: 18
                Layout.rightMargin: 18
                RowLayout {
                    Layout.fillWidth: true
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Label { text: "Default unlocker" }
                        Caption { text: "Preselected for games without an unlocker" }
                    }
                    ComboBox {
                        model: ["SmokeAPI", "CreamAPI"]
                        currentIndex: win.s.defaultUnlocker
                        onActivated: index => {
                            Backend.setDefaultUnlocker(index)
                            currentIndex = Qt.binding(() => win.s.defaultUnlocker)
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Label { text: "Default install method" }
                        Caption { text: "Koaloader applies to SmokeAPI on Windows games only" }
                    }
                    ComboBox {
                        model: ["Direct", "Koaloader"]
                        currentIndex: win.s.defaultMethod
                        onActivated: index => {
                            Backend.setDefaultMethod(index)
                            currentIndex = Qt.binding(() => win.s.defaultMethod)
                        }
                    }
                }
                RowLayout {
                    Layout.fillWidth: true
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Label { text: "Check for updates on startup" }
                        Caption { text: "Shows a link to the new release; nothing is downloaded" }
                    }
                    Switch {
                        checked: win.s.checkUpdates
                        onToggled: Backend.setCheckUpdates(checked)
                    }
                }
            }

            SectionTitle { text: "Folders"; Layout.leftMargin: 18 }
            Section {
                Layout.leftMargin: 18
                Layout.rightMargin: 18
                Repeater {
                    model: win.s.folders
                    RowLayout {
                        id: folderRow
                        required property var modelData
                        Layout.fillWidth: true
                        spacing: 8
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2
                            Label { text: folderRow.modelData.title }
                            Caption { text: folderRow.modelData.path; wrapMode: Text.WrapAnywhere }
                        }
                        Button {
                            text: "Change…"
                            onClicked: {
                                folderDialog.folder = folderRow.modelData.index
                                folderDialog.folderTitle = folderRow.modelData.title
                                folderDialog.currentFolder = folderRow.modelData.url
                                folderDialog.open()
                            }
                            ToolTip.visible: hovered
                            ToolTip.text: folderRow.modelData.tip
                        }
                        Button {
                            text: "Reset"
                            enabled: folderRow.modelData.custom
                            onClicked: Backend.resetFolder(folderRow.modelData.index)
                            ToolTip.visible: hovered
                            ToolTip.text: "Use the default folder again"
                        }
                    }
                }
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2
                    Label { text: "Settings are stored in" }
                    Caption { text: win.s.settingsPath; wrapMode: Text.WrapAnywhere }
                }
            }
            Caption {
                Layout.leftMargin: 18
                Layout.rightMargin: 18
                text: win.s.foldersNote
            }

            DimLabel {
                Layout.topMargin: 12
                Layout.leftMargin: 18
                Layout.rightMargin: 18
                Layout.bottomMargin: 18
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                text: win.v.status
            }
        }
    }
}
