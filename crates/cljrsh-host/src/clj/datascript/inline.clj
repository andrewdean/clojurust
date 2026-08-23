(ns datascript.inline
  (:refer-clojure :exclude [assoc update]))

;; cljrsh: the upstream file is a JVM fast path over clojure.lang.RT;
;; here core assoc/update are already the fast path.

(def assoc clojure.core/assoc)

(def update clojure.core/update)
