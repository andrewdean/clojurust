;; Phase-5 conformance gate: datalevin's own test suites (pinned 78b199e8,
;; EPL-2.0, vendored under src/ with :cljrsh gates only where the port
;; diverges) run against the native engine. Prints one summary line per
;; suite; any failure or error fails the case.
(require '[clojure.test :as t]
         '[datalevin.query-resolve-test]
         '[datalevin.test.index])
(defn gate [sym]
  (let [res (t/run-tests sym)]
    (println (str sym ":")
             (:test res) "tests," (:pass res) "assertions,"
             (:fail res) "failures," (:error res) "errors")
    (+ (:fail res) (:error res))))
(def bad (+ (gate 'datalevin.query-resolve-test)
            (gate 'datalevin.test.index)))
(when (pos? bad) (System/exit 1))
