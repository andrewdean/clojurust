#pragma once
#include <QObject>
#include <QString>
#include <QVariantMap>
#include <QtQml/qqmlregistration.h>

// C ABI from runtime/ (libcljrs_qt_runtime.so).
extern "C" {
void *cljrs_qt_new();
char *cljrs_qt_eval(void *rt, const char *code);
void cljrs_qt_free_str(char *s);
void cljrs_qt_destroy(void *rt);
typedef void (*cljrs_state_cb)(void *user, const char *key, const char *value_json);
void cljrs_qt_set_state_callback(void *rt, cljrs_state_cb cb, void *user);
unsigned short cljrs_qt_nrepl_port(const void *rt);
}

// A clj.rs runtime as a QML object.
//
// - `eval(code)` returns the runtime's JSON envelope ({"ok":...}/{"error":...})
//   as a string — JSON.parse on the QML side.
// - `source` evaluates a .cljrs file into the runtime, so later evals (and
//   nREPL sessions) can use what it defined.
// - `state` is a map fed from Clojure: every `(qml/set! :key value)` lands
//   here and emits stateChanged, so property bindings follow cljrs atoms.
//   Updates are queued onto the QML thread — qml/set! is safe from any
//   evaluating thread, the embedded nREPL's included.
// - `nreplPort`: the localhost port the runtime's always-on nREPL server
//   listens on. Evals from QML ride the same server, so a connected editor
//   shares the exact environment — defs from CIDER are live in QML. A REPL
//   into the embedding process: localhost trust model, know your host.
class CljrsRuntime : public QObject {
    Q_OBJECT
    QML_ELEMENT
    Q_PROPERTY(QString source READ source WRITE setSource NOTIFY sourceChanged)
    Q_PROPERTY(QVariantMap state READ state NOTIFY stateChanged)
    Q_PROPERTY(int nreplPort READ nreplPort CONSTANT)
public:
    explicit CljrsRuntime(QObject *parent = nullptr);
    ~CljrsRuntime() override;

    QString source() const { return m_source; }
    void setSource(const QString &path);

    QVariantMap state() const { return m_state; }

    int nreplPort() const { return m_nreplPort; }

    Q_INVOKABLE QString eval(const QString &code);

signals:
    void sourceChanged();
    void stateChanged();

private:
    static void stateCallback(void *user, const char *key, const char *valueJson);
    void applyState(const QString &key, const QByteArray &valueJson);

    void *m_rt = nullptr;
    QString m_source;
    QVariantMap m_state;
    int m_nreplPort = -1;
};
