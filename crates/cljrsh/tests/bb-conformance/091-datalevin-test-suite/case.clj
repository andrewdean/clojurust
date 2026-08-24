;; Phase-5 conformance gate: datalevin's own test suites (pinned 78b199e8,
;; EPL-2.0, vendored under src/) run against the native engine.
(require '[clojure.test :as t]
         '[datalevin.query-resolve-test])
(defn gate [sym]
  (let [res (t/run-tests (find-ns sym))]
    (println (str sym ":")
             (:test res) "tests" (:pass res) "assertions"
             (:fail res) "failures" (:error res) "errors")
    (+ (:fail res) (:error res))))
(def bad (+ (gate 'datalevin.query-resolve-test)))
(when (pos? bad) (System/exit 1))
