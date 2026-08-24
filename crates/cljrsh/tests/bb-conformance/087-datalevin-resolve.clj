(require '[datalevin.query.resolve :as qr]
         '[datalevin.query.aggregate :as qa]
         '[datalevin.db :as db]
         '[datalevin.storage :as s]
         '[datalevin.datom :as d]
         '[datalevin.parser :as dp]
         '[datalevin.query-util :as qu]
         '[datalevin.rules :as rules]
         '[babashka.fs :as fs])
(def dir (str (fs/create-temp-dir) "/resolve"))
(def dd (let [st (s/open dir)]
          (s/set-attr! st :friend {:cardinality-many true :ref true})
          (s/load-datoms st
            [(d/datom 1 :name "Ivan") (d/datom 1 :age 20)
             (d/datom 2 :name "Petr") (d/datom 2 :age 30)
             (d/datom 3 :name "Oleg") (d/datom 3 :age 40)
             (d/datom 1 :friend 2) (d/datom 2 :friend 3)])
          (db/new-db st)))
;; Input binding: scalar source, scalar, collection.
(def parsed (dp/parse-query '[:find ?n :in $ ?min [?x ...]
                              :where [?e :name ?n]]))
(def ctx (qr/resolve-ins {:parsed-q parsed :rels []} [dd 25 [10 20 30]]))
(prn (sort (map key (:sources ctx)))
     (mapv (fn [rel] [(:attrs rel) (mapv vec (:tuples rel))]) (:rels ctx)))
;; Pattern lookup: free vars, bound value, collection source.
(prn (sort-by first (mapv vec (:tuples (qr/lookup-pattern {:sources {'$ dd}}
                                                          dd '[?e :age ?a])))))
(prn (mapv vec (:tuples (qr/lookup-pattern {:sources {'$ dd}}
                                           dd '[?e :name "Petr"]))))
(prn (mapv vec (:tuples (qr/lookup-pattern {} [[1 :a 10] [2 :a 20]]
                                           '[?e :a ?v]))))
;; Clause resolution under the implicit source, as the driver binds it.
(def parsed-rules (rules/parse-rules
  '[[(reach ?a ?b) [?a :friend ?b]]
    [(reach ?a ?b) [?a :friend ?c] (reach ?c ?b)]]))
(def rctx {:sources {'$ dd} :rels [] :rules parsed-rules
           :rules-deps (rules/dependency-graph parsed-rules)})
(binding [qu/*implicit-source* dd]
  (let [base (qr/lookup-pattern {:sources {'$ dd}} dd '[?e :age ?a])]
    ;; predicate and function-binding clauses
    (prn (mapv vec (:tuples (first (:rels (qr/resolve-clause
                                            (assoc rctx :rels [base])
                                            '[(< ?a 35)]))))))
    (let [c (qr/resolve-clause (assoc rctx :rels [base]) '[(inc ?a) ?b])]
      (prn (:attrs (first (:rels c)))
           (sort-by first (mapv vec (:tuples (first (:rels c)))))))
    ;; or / not / not-join
    (prn (sort-by first (mapv vec (:tuples (first (:rels
      (qr/resolve-clause (assoc rctx :rels [base])
                         '(or [?e :name "Ivan"] [?e :name "Oleg"]))))))))
    (prn (sort-by first (mapv vec (:tuples (first (:rels
      (qr/resolve-clause (assoc rctx :rels [base])
                         '(not [?e :name "Petr"]))))))))
    (prn (sort-by first (mapv vec (:tuples (first (:rels
      (qr/resolve-clause (assoc rctx :rels [base])
                         '(not-join [?e] [?e :friend ?f]))))))))
    ;; recursive rules: transitive closure, unbound and bound.
    (prn (sort-by str (mapv vec (:tuples (first (:rels
      (qr/resolve-clause rctx '(reach ?x ?y))))))))
    (let [bound (qr/lookup-pattern {:sources {'$ dd}} dd '[?x :name "Ivan"])]
      (prn (sort-by str (mapv vec (:tuples (first (:rels
        (qr/resolve-clause (assoc rctx :rels [bound])
                           '(reach ?x ?y)))))))))))
;; Aggregates: one column per find element; grouping by non-aggregates.
(def fctx (dp/parse-query '[:find (count ?a) (max ?a) (min ?a) (sum ?a)
                            :where [?e :age ?a]]))
(prn (mapv vec (qa/aggregate (:elements (:qfind fctx)) {}
                             [(object-array [20 20 20 20])
                              (object-array [30 30 30 30])
                              (object-array [40 40 40 40])])))
(def gctx (dp/parse-query '[:find ?e (count ?a) :where [?e :x ?a]]))
(prn (sort-by first (mapv vec (qa/aggregate (:elements (:qfind gctx)) {}
                                            [(object-array [1 10])
                                             (object-array [1 20])
                                             (object-array [2 30])]))))
