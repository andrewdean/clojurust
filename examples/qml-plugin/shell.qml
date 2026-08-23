// Standalone QuickShell config proving the Cljrs QML module: run with
//   QML_IMPORT_PATH=<plugin build dir>/qml qs -p test/shell.qml
// Never load this into the live omarchy-shell.
import QtQuick
import Quickshell
import Cljrs

ShellRoot {
    CljrsRuntime {
        id: rt

        onStateChanged: console.log("CLJRS state:", JSON.stringify(state))

        Component.onCompleted: {
            console.log("CLJRS nrepl port:", rt.nreplPort)
            console.log("CLJRS eval:", rt.eval("(+ 1 2)"))
            console.log("CLJRS eval:", rt.eval(
                "(require '[clojure.string :as str]) (str/upper-case \"quickshell, meet clj.rs\")"))
            console.log("CLJRS eval:", rt.eval("(reduce + (range 101))"))
            console.log("CLJRS eval:", rt.eval("(undefined-fn 1)"))
            // Atom-driven state: a watcher pushes every change through
            // (qml/set! ...) into rt.state.
            console.log("CLJRS eval:", rt.eval(
                "(def counter (atom 0))" +
                "(add-watch counter :qml (fn [_ _ _ n] (qml/set! :count n)))" +
                "(qml/set! :count @counter)" +
                "(dotimes [_ 3] (swap! counter inc))" +
                "@counter"))
            exitTimer.start()
        }
    }

    // Give queued state updates a beat to land, then self-terminate —
    // quickshell has no QML quit API; Qt.quit() is unhandled.
    Timer {
        id: exitTimer
        interval: 1500
        onTriggered: {
            console.log("CLJRS final state:", JSON.stringify(rt.state))
            Quickshell.execDetached(["kill", String(Quickshell.processId)])
        }
    }
}
