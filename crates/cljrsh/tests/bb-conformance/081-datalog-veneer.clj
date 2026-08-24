(require '[cljrsh.datalog :as d]
         '[cljrs.dstore :as ds]
         '[babashka.fs :as fs])
;; collection tier: native engine, no pod fetch
(prn (sort (d/q '[:find ?n :where [?e :name ?n]] [[1 :name "x"] [2 :name "y"]])))
(prn (d/q '[:find ?n . :where [?e :kind "Pod"] [?e :name ?n]]
       (d/facts [{:kind "Pod" :name "sqlite"} {:kind "Tool" :name "jq"}])))
;; durable tier through the same veneer entry point
(def dir (str (fs/create-temp-dir) "/db"))
(def db (ds/open dir))
(ds/transact! db [[:add 1 :name "Ivan"] [:add 1 :age 39]])
(prn (d/q '[:find [?n ?a] :where [?e :name ?n] [?e :age ?a]] db))
;; mixed durable + collection sources in one query
(prn (d/q '[:find ?tag . :in $ $tags :where [?e :name ?n] [$tags ?n ?tag]]
       db [["Ivan" :admin]]))
(fs/delete-tree (fs/parent dir))
