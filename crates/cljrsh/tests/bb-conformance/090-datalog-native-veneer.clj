(require '[cljrsh.datalog :as d]
         '[babashka.fs :as fs])
;; Collection tier: vendored datascript, as before.
(prn (sort (d/q '[:find ?n :where [?e :name ?n]] [[1 :name "x"] [2 :name "y"]])))
(prn (d/q '[:find ?n . :where [?e :kind "Pod"] [?e :name ?n]]
       (d/facts [{:kind "Pod" :name "sqlite"} {:kind "Tool" :name "jq"}])))
;; Durable tier: the full native datalevin engine, no pod.
(def dir (str (fs/create-temp-dir) "/db"))
(def c (d/conn dir {:friend {:db/cardinality :db.cardinality/many
                             :db/valueType :db.type/ref}}))
(def rep (d/transact! c [{:name "Ivan" :age 20}
                         {:db/id -2 :name "Petr" :age 30}
                         [:db/add -2 :friend 1]
                         {:db/id 3 :name "Oleg" :age 40 :friend [1 -2]}]))
(prn (:tempids rep) (count (:tx-data rep)))
(prn (d/q '[:find ?n ?a :where [?e :name ?n] [?e :age ?a]] (d/db c)))
(prn (d/q '[:find ?n ?fn :where [?e :name ?n] [?e :friend ?f]
            [?f :name ?fn]] (d/db c)))
(prn (d/pull (d/db c) '[:name {:friend [:name]}] 3))
(prn (d/entity (d/db c) 1))
;; Retraction and rules through the same veneer.
(d/transact! c [[:db/retract 1 :age 20]])
(prn (d/q '[:find ?a . :in $ ?e :where [?e :age ?a]] (d/db c) 1))
(prn (d/q '[:find ?a ?b :in $ % :where (reach ?a ?b)] (d/db c)
          '[[(reach ?a ?b) [?a :friend ?b]]
            [(reach ?a ?b) [?a :friend ?c] (reach ?c ?b)]]))
;; Mixed durable + collection sources in one query.
(prn (d/q '[:find ?tag . :in $ $tags :where [?e :name ?n] [$tags ?n ?tag]]
       (d/db c) [["Petr" :admin]]))
;; Reopen: the data is durable.
(d/close c)
(def c2 (d/conn dir))
(prn (d/q '[:find ?n . :in $ ?e :where [?e :name ?n]] (d/db c2) 3))
(fs/delete-tree (fs/parent dir))
