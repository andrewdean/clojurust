(defn boom [] (throw (ex-info "exploded" {:why :test})))
(boom)
