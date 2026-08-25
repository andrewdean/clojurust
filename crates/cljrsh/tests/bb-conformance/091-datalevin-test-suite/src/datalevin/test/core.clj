;;
;; cljrs replacement for datalevin's test harness namespace (upstream
;; test/datalevin/test/core.clj at 78b199e8, EPL-2.0, Copyright (c)
;; Huahai Yang): just the pieces the vendored suites use — the entity
;; facade, timbre logging, and server-socket helpers are not ported.
;;
(ns datalevin.test.core
  (:require
   [datalevin.constants :as c]
   [datalevin.core :as d]))

(defn db-fixture
  [f]
  (binding [c/*db-background-sampling?* false]
    (f)))

(defn all-datoms [db]
  (into #{} (map (juxt :e :a :v)) (d/datoms db :eav)))
