(require '[datalevin.db :as db]
         '[datalevin.datom :as d]
         '[datalevin.storage :as s]
         '[datalevin.interface :as i]
         '[babashka.fs :as fs])
(def dir (str (fs/create-temp-dir) "/db"))
(def d0 (db/empty-db dir {:friend {:db/cardinality :db.cardinality/many
                                   :db/valueType :db.type/ref}}))
(s/load-datoms (:store d0)
  [(d/datom 1 :name "Ivan") (d/datom 1 :age 20)
   (d/datom 2 :name "Petr") (d/datom 2 :age 30)
   (d/datom 3 :name "Oleg") (d/datom 3 :age 40)
   (d/datom 1 :friend 2) (d/datom 1 :friend 3) (d/datom 2 :friend 3)])
(def dd (db/new-db (:store d0)))
;; Search case tree.
(prn (mapv d/datom-eav (db/-search dd [1 :name nil])))
(prn (mapv d/datom-eav (db/-search dd [nil :age 30])))
(prn (mapv d/datom-eav (db/-search dd [nil :age nil])))
(prn (mapv d/datom-eav (db/-search dd [1 nil 20])))
(prn (mapv d/datom-eav (db/-search dd [nil nil 40])))
;; Count case tree (O(log n) rank counts underneath).
(prn [(db/-count dd [1 nil nil]) (db/-count dd [nil :age nil])
      (db/-count dd [nil nil nil]) (db/-count dd [nil :friend 3])])
;; Index access.
(prn (mapv d/datom-eav (db/-datoms dd :ave :age)))
(prn (mapv d/datom-eav (db/-rseek-datoms dd :ave :age nil nil 2)))
(prn (db/-populated? dd :eav 1 :name nil) (db/-populated? dd :eav 9 nil nil))
(prn (db/-cardinality dd :friend) (db/-index-range-size dd :age 25 45))
(prn (mapv d/datom-eav (db/-index-range dd :age 25 45)))
;; Tuple pipeline.
(prn (mapv vec (db/-init-tuples-list dd :age [[[:closed 25] [:closed 45]]]
                                     nil true)))
(prn (mapv vec (db/-eav-scan-v-list dd [(object-array [1])
                                        (object-array [3])] 0 [[:name {}]])))
(prn (mapv vec (db/-val-eq-scan-e-list dd [(object-array ["Oleg"])] 0 :name)))
(prn (mapv vec (db/-search-tuples dd [nil :age nil])))
;; Schema helpers and searchable dispatch (incl. the Object/nil default).
(prn (db/-searchable? dd) (db/-searchable? 42) (db/-searchable? nil))
(prn [(db/ref? dd :friend) (db/multival? dd :friend) (db/multival? dd :name)
      (db/entid dd 5) (db/reverse-ref? :_friend) (db/reverse-ref :friend)
      (db/max-eid dd)])
;; The store also answers through the datalevin.interface protocols.
(prn (i/av-first-e (:store dd) :name "Petr")
     (sort (:db.cardinality/many (i/rschema (:store dd))))
     (i/datom-count (:store dd) :eav))
;; Reopen: data survives.
(def dd2 (db/new-db (s/open dir)))
(prn (db/-count dd2 [nil nil nil]) (d/datom-eav (db/-first dd2 [nil :name "Ivan"])))
