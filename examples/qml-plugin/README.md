# QML plugin example — cljrs inside a Qt/QML host

The consumer side of [`crates/cljrs-qt`](../../crates/cljrs-qt): a thin C++
QML extension module exposing the runtime as a `CljrsRuntime` type, plus a
[QuickShell](https://quickshell.org/) proof config. Built for driving
Omarchy/QuickShell shell widgets from Clojure, but the module is plain Qt —
any QML host works.

```qml
import Cljrs

CljrsRuntime {
    id: rt
    source: "widget.cljrs"          // evaluated into the runtime
    onStateChanged: ...             // fed by (qml/set! :key value)
}
Text { text: rt.state.count }
// rt.eval("(+ 1 2)")  => '{"ok":3}'   (JSON.parse on the QML side)
// rt.nreplPort        => connect CIDER; same environment as the widget
```

The runtime is nREPL-centered: one interpreter thread serves both QML evals
(as a localhost bencode client) and editor connections, so defs made from
CIDER are live in the widget — including atom mutations, which flow through
watchers and `qml/set!` into the `state` property. See the crate docs for
the architecture and the localhost trust caveat.

## Build & prove

```bash
cargo build --release -p cljrs-qt          # workspace root

cmake -S examples/qml-plugin -B examples/qml-plugin/build -DCMAKE_BUILD_TYPE=Release
cmake --build examples/qml-plugin/build
mkdir -p examples/qml-plugin/build/qml/Cljrs
ln -sf ../../qmldir ../../libcljrs_qml.so ../../cljrs_qml.qmltypes \
      examples/qml-plugin/build/qml/Cljrs/

# Eval + state-push proof (self-terminates):
QML_IMPORT_PATH=$PWD/examples/qml-plugin/build/qml qs -p examples/qml-plugin/shell.qml

# Shared-environment proof: leaves the shell running; connect to the logged
# port with CIDER or the bundled client and mutate the widget's atom:
QML_IMPORT_PATH=$PWD/examples/qml-plugin/build/qml qs -p examples/qml-plugin/shared-env.qml &
python3 examples/qml-plugin/nrepl-client.py <port> '(swap! counter inc)'
```

Native QML plugins must match the host's Qt major/ABI — rebuild alongside
Qt upgrades, same as any Qt plugin.
