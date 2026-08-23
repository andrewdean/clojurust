(prn (re-find #"\d+" "abc123def"))
(prn (re-matches #"[a-z]+" "abc"))
(prn (re-seq #"\w+" "a b c"))
(prn (re-find #"(\d+)-(\d+)" "10-20"))
