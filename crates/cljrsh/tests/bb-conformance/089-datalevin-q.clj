(require '[datalevin.query :as q]
         '[datalevin.pull-api :as dpa]
         '[datalevin.db :as db]
         '[datalevin.storage :as s]
         '[datalevin.datom :as d]
         '[babashka.fs :as fs])
(def dir (str (fs/create-temp-dir) "/qdb"))
(def dd (let [st (s/open dir)]
          (s/set-attr! st :friend {:cardinality-many true :ref true})
          (s/load-datoms st
            [(d/datom 1 :name "Ivan") (d/datom 1 :age 20)
             (d/datom 2 :name "Petr") (d/datom 2 :age 30)
             (d/datom 3 :name "Oleg") (d/datom 3 :age 40)
             (d/datom 1 :friend 2) (d/datom 2 :friend 3)])
          (db/new-db st)))
;; The full datalevin query pipeline — parse, plan, execute — natively
;; against the Rust store.
(prn (q/q '[:find ?n ?a :where [?e :name ?n] [?e :age ?a]] dd))
(prn (q/q '[:find ?n ?fn :where [?e :name ?n] [?e :friend ?f]
            [?f :name ?fn]] dd))
(prn (q/q '[:find ?n :where [?e :age ?a] [(< ?a 35)] [?e :name ?n]] dd))
(prn (q/q '[:find ?n :in $ ?min :where [?e :age ?a] [(>= ?a ?min)]
            [?e :name ?n]] dd 30))
(prn (q/q '[:find ?n . :where [?e :age 40] [?e :name ?n]] dd))
(prn (sort (q/q '[:find [?n ...] :where [?e :name ?n]] dd)))
(prn (q/q '[:find [?n ?a] :where [?e :name ?n] [?e :age ?a]
            [(= ?n "Petr")]] dd))
(prn (q/q '[:find (count ?e) (max ?a) (min ?a) (sum ?a)
            :where [?e :age ?a]] dd))
(prn (q/q '[:find ?n :where [?e :name ?n] (not [?e :friend _])] dd))
(prn (q/q '[:find ?n :where [?e :name ?n]
            (or [?e :age 20] [?e :age 40])] dd))
(prn (q/q '[:find ?n :where [?e :name ?n]
            (not-join [?e] [?e :friend ?f])] dd))
;; Recursive rules through the whole pipeline.
(prn (q/q '[:find ?a ?b :in $ % :where (reach ?a ?b)] dd
          '[[(reach ?a ?b) [?a :friend ?b]]
            [(reach ?a ?b) [?a :friend ?c] (reach ?c ?b)]]))
;; Function bindings, ordering, windows.
(prn (q/q '[:find ?n ?b :where [?e :name ?n] [?e :age ?a]
            [(inc ?a) ?b]] dd))
(prn (q/q '[:find ?n ?a :order-by [?a :desc]
            :where [?e :name ?n] [?e :age ?a]] dd))
(prn (q/q '[:find ?n ?a :order-by [?a :asc] :limit 2
            :where [?e :name ?n] [?e :age ?a]] dd))
(prn (q/q '[:find ?n ?a :order-by [?a :asc] :limit 2 :offset 1
            :where [?e :name ?n] [?e :age ?a]] dd))
;; Pull: attr lists, nested refs, wildcard, and pull find-specs.
(prn (dpa/pull dd [:name :age] 1))
(prn (dpa/pull dd [:name {:friend [:name]}] 1))
(prn (dpa/pull dd '[*] 3))
(prn (q/q '[:find (pull ?e [:name :age]) :where [?e :age 20]] dd))
;; Collection sources still work through the same facade.
(prn (q/q '[:find ?v :where [_ :x ?v]] [[1 :x 10] [2 :x 20]]))
