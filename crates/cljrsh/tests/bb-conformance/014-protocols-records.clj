(defprotocol Greet (hello [this]))
(defrecord Person [name]
  Greet
  (hello [_] (str "hi " name)))
(prn (hello (->Person "ada")))
(prn (:name (map->Person {:name "grace"})))
(prn (satisfies? Greet (->Person "x")))
