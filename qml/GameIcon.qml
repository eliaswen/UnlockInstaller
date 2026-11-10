// A game's Steam icon, or its initial until the icon has been fetched.

import QtQuick
import QtQuick.Controls
import UnlockInstaller

Control {
    id: icon
    property int appid
    property string name
    property int size: 40
    property string url: ""
    implicitWidth: size
    implicitHeight: size
    padding: 0

    onAppidChanged: url = appid ? Backend.gameIcon(appid) : ""
    Component.onCompleted: url = appid ? Backend.gameIcon(appid) : ""

    Connections {
        target: Backend
        function onIconReady(id, url) {
            if (id === icon.appid)
                icon.url = url
        }
    }

    Rectangle {
        anchors.fill: parent
        visible: img.status !== Image.Ready
        radius: icon.size / 6
        color: Qt.rgba(icon.palette.windowText.r, icon.palette.windowText.g, icon.palette.windowText.b, 0.1)
        Label {
            anchors.centerIn: parent
            text: icon.name.charAt(0).toUpperCase()
            font.pixelSize: icon.size / 2
            opacity: 0.6
        }
    }
    Image {
        id: img
        anchors.fill: parent
        source: icon.url
        sourceSize.width: icon.size * 2
        sourceSize.height: icon.size * 2
        fillMode: Image.PreserveAspectFit
        smooth: true
        mipmap: true
        asynchronous: true
    }
}
