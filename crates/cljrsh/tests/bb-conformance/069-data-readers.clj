(binding [*data-readers* {'conf/double (fn [v] (* 2 v))}]
  (prn #conf/double 21))
