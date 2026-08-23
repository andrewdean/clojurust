(require '[datascript.query :as dq]
         '[datascript.db :as db]
         '[datascript.pull-api :as dp])
(prn (dq/q '[:find ?n ?a :where [?e :name ?n] [?e :age ?a]]
       [[1 :name "Ivan"] [1 :age 39] [2 :name "Petr"] [2 :age 22]]))
(prn (dq/q '[:find ?e :where [?e :age ?a] [(>= ?a 30)]] [[1 :age 39] [2 :age 22]]))
(prn (dq/q '[:find ?x :in [?x ...]] [1 2 3]))
(def d1 (:db-after
          (db/transact-tx-data
            (db/map->TxReport {:db-before (db/empty-db {:name {:db/unique :db.unique/identity}
                                                        :friend {:db/valueType :db.type/ref}} {})
                               :db-after (db/empty-db {:name {:db/unique :db.unique/identity}
                                                       :friend {:db/valueType :db.type/ref}} {})
                               :tx-data [] :tempids {} :tx-meta nil})
            [{:db/id 1 :name "Ivan" :age 39}
             {:db/id 2 :name "Petr" :age 22 :friend 1}])))
(prn (dq/q '[:find ?fn . :where [?e :name "Petr"] [?e :friend ?f] [?f :name ?fn]] d1))
(prn (dp/pull d1 '[:name] 1))
