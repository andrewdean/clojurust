(require '[cljrsh.datalog :as d]
         '[babashka.fs :as fs])
;; Pulling a pattern that names a never-asserted attribute must return the
;; known attributes, not nil the whole pull. Regression: an unknown attr has
;; no aid, and anchoring the eav range scan on it silently emptied the range
;; (surfaced by swarmd's taskstored pulling :task/claimed-by on fresh tasks).
(def dir (str (fs/create-temp-dir) "/db"))
(def c (d/conn dir {:task/blocked-by {:db/cardinality :db.cardinality/many
                                      :db/valueType :db.type/ref}}))
(d/transact! c [{:db/id -1 :task/id "a" :task/title "A" :task/state "open"}])
(d/transact! c [{:db/id -1 :task/id "b" :task/title "B" :task/state "open"}
                [:db/add -1 :task/blocked-by 1]])
;; Unknown attr sorts first (nil aid) — the old bug anchored the range on it.
(prn (d/pull (d/db c) '[:task/claimed-by :task/id :task/title] 2))
;; Unknown attr sandwiched between known ones.
(prn (d/pull (d/db c) '[:task/id :task/never-set :task/state] 1))
;; Unknown attrs inside a nested ref pattern.
(prn (d/pull (d/db c) '[:task/id {:task/blocked-by [:task/id :task/branch]}] 2))
;; A :default on an unknown attr still applies.
(prn (d/pull (d/db c) '[:task/id [:task/priority :default 0]] 1))
(d/close c)
(fs/delete-tree (fs/parent dir))
