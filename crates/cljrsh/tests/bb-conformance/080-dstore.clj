(require '[cljrs.dstore :as ds]
         '[babashka.fs :as fs])
(def dir (str (fs/create-temp-dir) "/db"))
(def db (ds/open dir))
(ds/set-attr! db :friend {:cardinality-many true :ref true})
(ds/transact! db [[:add 1 :name "Ivan"] [:add 1 :age 39]
                  [:add 2 :name "Petr"] [:add 2 :age 22]
                  [:add 2 :friend 1] [:add 3 :name "Oleg"] [:add 2 :friend 3]])
(prn (sort (ds/q '[:find ?n ?a :where [?e :name ?n] [?e :age ?a]] db)))
(prn (ds/q '[:find ?n . :where [?e :age ?a] [(>= ?a 30)] [?e :name ?n]] db))
(prn (ds/q '[:find ?fn :where [?e :name "Petr"] [?e :friend ?f] [?f :name ?fn]] db))
(prn (ds/pull db '[:name {:friend [:name]}] 2))
(prn (ds/count-datoms db nil :name nil))
;; cardinality-one replacement is durable behavior, not just in-memory
(ds/transact! db [[:add 1 :age 40]])
(prn (ds/q '[:find ?a . :where [1 :age ?a]] db))
(prn (ds/count-datoms db 1 :age nil))
;; a second handle reads the same data from disk
(def db2 (ds/open dir))
(prn (ds/q '[:find ?n . :where [3 :name ?n]] db2))
(fs/delete-tree (fs/parent dir))
