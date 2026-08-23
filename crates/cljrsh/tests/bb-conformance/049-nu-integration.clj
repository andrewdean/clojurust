(prn (nu/eval "3 + 4"))
(prn (nu/eval "[[n]; [1] [2]] | get n | math sum"))
(prn (nu/eval "$in | str upcase" {:in "shout"}))
