(require '[datalevin.relation :as r]
         '[datalevin.timeout :as timeout]
         '[datalevin.query.predicate :as qp]
         '[datalevin.query.tuple :as qt]
         '[datalevin.parser :as dp])
;; Relation algebra over object-array tuples.
(def rel1 (r/relation! '{?e 0 ?n 1}
                       [(object-array [1 "Ivan"]) (object-array [2 "Petr"])]))
(def rel2 (r/relation! '{?e 0 ?n 1}
                       [(object-array [2 "Petr"]) (object-array [3 "Oleg"])]))
(prn (mapv vec (:tuples (r/sum-rel rel1 rel2))))
(prn (mapv vec (:tuples (r/sum-rel-dedupe rel1 rel2))))
(prn (mapv vec (:tuples (r/difference rel1 rel2))))
;; Same keys, different column order: sum renumbers and remaps.
(prn (mapv vec (:tuples (r/sum-rel rel1 (r/relation! '{?n 0 ?e 1}
                                                     [(object-array ["Oleg" 3])])))))
(prn (mapv vec (:tuples (r/prod-rel
                          (r/relation! '{?a 0} [(object-array [1]) (object-array [2])])
                          (r/relation! '{?b 0} [(object-array [10])])))))
(prn (mapv vec (:tuples (r/project-distinct rel1 '[?n]))))
(prn (mapv vec (r/many-tuples [[1 2] [10]])))
(let [seen (r/new-seen-set)]
  (prn (mapv vec (:tuples (r/difference-with-seen! rel1 seen)))
       (mapv vec (:tuples (r/difference-with-seen! rel2 seen)))))
(prn (mapv vec (r/select-tuples #(= 2 (aget % 0)) (:tuples rel1))))
(prn (r/rel-empty (r/relation! {} [])) (r/rel-not-empty rel1))
(prn (vec (r/join-tuples (object-array [1 2]) (object-array [3]))))
;; An expired deadline aborts relation construction.
(prn (binding [timeout/*deadline* 1]
       (try (r/relation! {} []) (catch Exception e (:type (ex-data e))))))
;; Tuple-binding projection compiles flat bindings; nested/duplicate -> nil.
(def proj (qt/tuple-binding-projection (dp/parse-binding '[?e _ ?v])))
(prn (:cols proj) (:attrs proj) (some-> (:needed proj) vec)
     (:source-width proj) (:output-width proj))
(prn (qt/tuple-binding-projection (dp/parse-binding '[?x ?x])))
(def emit (qt/make-datom-emitter nil {10 :name}
                                 (qt/needed-indices (dp/parse-binding '[?e _ ?v]))))
(prn (vec (emit [7 10 "Ivan"])) (vec (emit [7 10 nil] "Petr")))
(prn (vec ((qt/make-datom-emitter nil {10 :name} nil) [7 10 "Ivan"])))
;; Forkable predicates preserve their factories through combination.
(def p1 (qp/shareable-predicate odd?))
(prn (qp/forkable-predicate? p1) (qp/forkable-predicate? even?)
     (qp/forkable-predicate? nil))
(def c (qp/combine-predicates p1 (qp/shareable-predicate #(< % 3)) false))
(prn (c 1) (c 4) (qp/forkable-predicate? c))
(prn (mapv #(% 1) (vec (qp/fork-predicates (object-array [p1 c])))))
