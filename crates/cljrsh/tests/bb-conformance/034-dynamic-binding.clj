(def ^:dynamic *level* 1)
(defn show [] (println *level*))
(show)
(binding [*level* 2]
  (show)
  (binding [*level* 3] (show))
  (show))
(show)
(binding [*level* 10]
  (println (doall (map (fn [x] (+ x *level*)) [1 2]))))
