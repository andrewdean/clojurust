(require '[malli.core :as m]
         '[cljrsh.config :as config]
         '[cljrsh.datalog :as dl])
(prn (m/validate [:map [:x :int]] {:x 1}))
(prn (m/validate [:cat :int :string] [1 "a"]))
(def Schema [:map
             [:replicas {:default 1} :int]
             [:name :string]
             [:env {:default "dev"} [:enum "dev" "prod"]]])
(prn (config/load-config [{:name "web"} {:replicas "3"}] Schema))
(prn (try (config/merge-layers {:a {:b 1}} {:a {:b 2}})
          (catch Exception e (:path (ex-data e)))))
;; datalog's pure helper (no pod needed)
(prn (dl/facts [{:k 1}]))
