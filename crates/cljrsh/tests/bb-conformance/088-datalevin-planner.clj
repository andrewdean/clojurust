(require '[datalevin.query-optimizer :as qo]
         '[datalevin.query.plan :as qplan]
         '[datalevin.query.access :as qacc]
         '[datalevin.query.optimizer.range :as qor]
         '[datalevin.db :as db]
         '[datalevin.storage :as s]
         '[datalevin.datom :as d]
         '[datalevin.parser :as dp]
         '[babashka.fs :as fs])
(def dir (str (fs/create-temp-dir) "/plandb"))
(def dd (let [st (s/open dir)]
          (s/set-attr! st :friend {:cardinality-many true :ref true})
          (s/load-datoms st
            [(d/datom 1 :name "Ivan") (d/datom 1 :age 20)
             (d/datom 2 :name "Petr") (d/datom 2 :age 30)
             (d/datom 3 :name "Oleg") (d/datom 3 :age 40)
             (d/datom 1 :friend 2) (d/datom 2 :friend 3)])
          (db/new-db st)))
;; Graph building with predicate pushdown over the durable store.
(def parsed (dp/parse-query '[:find ?n ?a
                              :where [?e :name ?n] [?e :age ?a] [(< ?a 35)]]))
(def gctx (qo/build-graph {:parsed-q parsed :sources {'$ dd} :rels []
                           :graph? true}))
(prn (sort (keys gctx)))
(prn (some->> gctx :graph vals first keys sort))
;; DPK: the dynamic-programming join-order key.
(def k1 (qo/->DPK 2 [1] true))
(def k2 (.append k1 3))
(prn (.contains k2 1) (.contains k2 3) (.contains k2 2)
     (.members k2) (.isOrdered k2))
(prn (= (qo/->DPK 10 nil false) (qo/->DPK 10 nil false))
     (= k2 (.append k1 3)) (= k2 (.append k1 4)))
;; Like-pattern helpers survive without the JVM FSM.
(prn (some? qor/pushdown-predicates))
;; writing? is statically false on cljrs: parallel branches collapse.
(prn (qo/writing? dd))
;; Plan namespace surface.
(prn (some? qplan/map->Node) (some? qplan/->Link) (some? qacc/source-symbol))
