#include "cljrsruntime.h"
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QMetaObject>

CljrsRuntime::CljrsRuntime(QObject *parent) : QObject(parent) {
    m_rt = cljrs_qt_new();
    if (m_rt) {
        cljrs_qt_set_state_callback(m_rt, &CljrsRuntime::stateCallback, this);
        m_nreplPort = cljrs_qt_nrepl_port(m_rt);
    }
}

CljrsRuntime::~CljrsRuntime() {
    if (m_rt) {
        cljrs_qt_set_state_callback(m_rt, nullptr, nullptr);
        cljrs_qt_destroy(m_rt);
    }
}

QString CljrsRuntime::eval(const QString &code) {
    if (!m_rt)
        return QStringLiteral("{\"error\":\"runtime failed to initialise\"}");
    const QByteArray utf8 = code.toUtf8();
    char *res = cljrs_qt_eval(m_rt, utf8.constData());
    QString out = res ? QString::fromUtf8(res)
                      : QStringLiteral("{\"error\":\"null result\"}");
    if (res) cljrs_qt_free_str(res);
    return out;
}

void CljrsRuntime::setSource(const QString &path) {
    if (path == m_source) return;
    m_source = path;
    QFile f(path);
    if (f.open(QIODevice::ReadOnly))
        eval(QString::fromUtf8(f.readAll()));
    emit sourceChanged();
}

// May fire on any evaluating thread (nREPL included); queue onto the object's
// thread — Qt properties and signal emission are not thread-safe.
void CljrsRuntime::stateCallback(void *user, const char *key, const char *valueJson) {
    auto *self = static_cast<CljrsRuntime *>(user);
    if (!self || !key) return;
    const QString k = QString::fromUtf8(key);
    const QByteArray v(valueJson ? valueJson : "null");
    QMetaObject::invokeMethod(
        self, [self, k, v] { self->applyState(k, v); }, Qt::QueuedConnection);
}

void CljrsRuntime::applyState(const QString &key, const QByteArray &valueJson) {
    // The runtime sends a bare JSON value; wrap it so QJsonDocument (which
    // only parses objects/arrays at the top level) can decode any scalar.
    const QJsonDocument doc = QJsonDocument::fromJson("[" + valueJson + "]");
    m_state.insert(key, doc.isArray() ? doc.array().at(0).toVariant() : QVariant());
    emit stateChanged();
}
