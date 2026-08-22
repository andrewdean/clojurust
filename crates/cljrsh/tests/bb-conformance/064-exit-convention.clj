(println "before")
(throw (ex-info "fatal: see log" {:babashka/exit 3}))
