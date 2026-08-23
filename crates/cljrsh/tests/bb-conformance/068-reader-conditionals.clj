(prn #?(:bb :bb-branch :default :other))
(prn #?(:cljs :js :clj :clj-branch))
(prn [#?@(:bb [1 2]) 3])
