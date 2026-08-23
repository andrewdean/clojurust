// Companion proof: leaves the shell running so an external nREPL client
// (CIDER, or test/nrepl-client.py) can connect to the logged port and see
// the same environment — `counter` defined here must be visible there.
import QtQuick
import Quickshell
import Cljrs

ShellRoot {
    CljrsRuntime {
        id: rt
        onStateChanged: console.log("CLJRS state:", JSON.stringify(state))
        Component.onCompleted: {
            console.log("CLJRS nrepl port:", rt.nreplPort)
            console.log("CLJRS eval:", rt.eval(
                "(def counter (atom 41))" +
                "(add-watch counter :qml (fn [_ _ _ n] (qml/set! :count n)))" +
                "@counter"))
        }
    }
}
