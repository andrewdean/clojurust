(require '[datalevin.datom :as dd]
         '[datalevin.constants :as c]
         '[datalevin.util :as u])
(prn c/tx0)
(prn c/emax)
(def d1 (dd/datom 1 :name "Ivan"))
(prn (dd/datom-eav d1))
(prn (dd/datom-tx d1) (dd/datom-added d1))
(prn (dd/cmp-datoms-eavt d1 (dd/datom 2 :name "Petr")))
(prn (neg? (dd/cmp-datoms-avet (dd/datom 5 :age 22) (dd/datom 3 :name "x"))))
(prn (dd/datom-eav (dd/delete d1)) (dd/datom-added (dd/delete d1)))
(prn (u/distinct-by :k [{:k 1 :n "a"} {:k 1 :n "b"} {:k 2 :n "c"}]))
(binding [u/*reservoir-sampling-seed* 42]
  (let [s1 (u/reservoir-sampling 100 5)
        s2 (u/reservoir-sampling 100 5)]
    (prn (= s1 s2) (count s1) (= s1 (sort s1)))))
(require '[datalevin.parser :as dp] '[datalevin.interface])
(let [pq (dp/parse-query '[:find ?n ?a :in $ ?min
                           :where [?e :name ?n] [?e :age ?a] [(>= ?a ?min)]])]
  (prn (mapv (comp str type) (:qwhere pq)))
  (prn (dp/find-vars (:qfind pq)))
  (prn (count (:qin pq))))
(prn (some? (dp/parse-rules '[[(person ?e) [?e :person true]]])))
