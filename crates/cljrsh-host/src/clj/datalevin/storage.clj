;;
;; cljrs replacement for datalevin.storage (upstream is 4k lines of
;; JVM/LMDB serde the Rust store already owns). It implements the
;; storage surface the query family consumes — the fn names, argument
;; shapes, and semantics follow upstream datalevin 78b199e8 storage.clj
;; (EPL-2.0, Copyright (c) Huahai Yang), but the bodies run against
;; cljrs.dstore.native. Datoms cross as datalevin.datom records; bounds
;; translate c/e0, c/emax, c/v0, c/vmax sentinels to open-ended native
;; range bounds. Loaded on :cljrsh only.
;;
(ns ^:no-doc datalevin.storage
  "Storage layer of Datalog store"
  (:require
   [cljrs.dstore.native :as native]
   [datalevin.constants :as c]
   [datalevin.interface :as i]
   [datalevin.datom :as d]
   [datalevin.pipe :as p]
   [datalevin.relation :as r]
   [datalevin.util :as u :refer [raise]]))

;; ── store handle ─────────────────────────────────────────────────────

(defrecord Store [handle dir opts])

(defn open
  "Open (creating if needed) a store in the directory at DIR."
  ([dir] (open dir {}))
  ([dir opts]
   (->Store (native/open (str dir)) (str dir) (or opts {}))))

(defn store? [x] (instance? Store x))

(defn handle [store] (:handle store))

(defn dir [store] (:dir store))

(defn db-name [store] (:dir store))

(defn opts [store] (:opts store))

(defn close [_store] nil)

(defn closed? [_store] false)

(defn last-modified [_store] 0)

(defn max-tx [_store] 0)

(defn set-attr!
  "Declare attribute properties: {:cardinality-many bool, :ref bool}."
  [store attr props]
  (native/set-attr! (:handle store) attr props))

;; ── schema ───────────────────────────────────────────────────────────

(defn- native-attrs [store] (native/attrs (:handle store)))

(defn schema
  "attr -> {:db/aid n, :db/cardinality ..., :db/valueType ...}. Aids are
  synthesized as positions in sorted attr order: stable per store state,
  used only for grouping and ordering."
  [store]
  (let [as (sort-by (comp str key) (native-attrs store))]
    (into {}
          (map-indexed
            (fn [i [a props]]
              [a (cond-> {:db/aid i
                          :db/cardinality (if (:cardinality-many props)
                                            :db.cardinality/many
                                            :db.cardinality/one)}
                   (:ref props) (assoc :db/valueType :db.type/ref))]))
          as)))

(defn rschema
  "property -> #{attrs}."
  [store]
  (let [sch (schema store)]
    {:db.cardinality/many
     (set (keep (fn [[a p]] (when (= :db.cardinality/many
                                    (:db/cardinality p)) a)) sch))
     :db.type/ref
     (set (keep (fn [[a p]] (when (= :db.type/ref (:db/valueType p)) a))
                sch))
     :db/unique #{}
     :db.unique/identity #{}
     :db.unique/value #{}}))

(defn attrs
  "aid -> attr."
  [store]
  (into {} (map (fn [[a p]] [(:db/aid p) a])) (schema store)))

;; ── datoms and bounds ────────────────────────────────────────────────

(defn- ->datom [[e a v]] (d/datom e a v))

(defn- ->datoms [ds] (mapv ->datom ds))

(defn- bound-e [e]
  (when (and e (not= e c/e0) (not= e c/emax)) (long e)))

(defn- bound-v [v]
  (when-not (or (nil? v) (= v c/v0) (= v c/vmax)) v))

(defn- datom-bound
  "Translate a partial datom into the native [e a v] bound for INDEX,
  dropping everything after the first unbounded component in the
  index's significance order."
  [index datom]
  (let [e (bound-e (d/datom-e datom))
        a (d/datom-a datom)
        v (bound-v (d/datom-v datom))]
    (case index
      :eav [e (when e a) (when (and e a) v)]
      :ave [(when (and a v) e) a (when a v)]
      :vae [(when (and v a) e) (when v a) v])))

(defn- norm-index [index]
  (case index
    (:eav :eavt) :eav
    (:ave :avet) :ave
    (:vae :vaet) :vae))

;; ── ranges and counts ────────────────────────────────────────────────

(defn slice
  "Datoms within [low-datom high-datom] (inclusive) on INDEX, in index
  order, optionally capped at N."
  ([store index low-datom high-datom]
   (slice store index low-datom high-datom nil))
  ([store index low-datom high-datom n]
   (let [index (norm-index index)]
     (->datoms (native/slice (:handle store) index
                             (datom-bound index low-datom)
                             (datom-bound index high-datom)
                             n)))))

(defn rslice
  "Datoms within [high-datom low-datom] on INDEX in reverse order,
  optionally capped at N."
  ([store index high-datom low-datom]
   (rslice store index high-datom low-datom nil))
  ([store index high-datom low-datom n]
   (let [index (norm-index index)]
     (->datoms (native/rslice (:handle store) index
                              (datom-bound index low-datom)
                              (datom-bound index high-datom)
                              n)))))

(defn size
  "O(log n) count of datoms within the range (inclusive)."
  [store index low-datom high-datom]
  (let [index (norm-index index)]
    (native/count-range (:handle store) index
                        (datom-bound index low-datom)
                        (datom-bound index high-datom))))

(defn populated?
  [store index low-datom high-datom]
  (pos? (long (size store index low-datom high-datom))))

(defn head
  [store index low-datom high-datom]
  (first (slice store index low-datom high-datom 1)))

(defn tail
  [store index high-datom low-datom]
  (first (rslice store index high-datom low-datom 1)))

;; The -filter variants scan the range and keep datoms PRED accepts
;; (PRED returns the datom or logical truth).

(defn slice-filter
  [store index pred low-datom high-datom]
  (into [] (filter #(pred %)) (slice store index low-datom high-datom)))

(defn rslice-filter
  [store index pred high-datom low-datom]
  (into [] (filter #(pred %)) (rslice store index high-datom low-datom)))

(defn head-filter
  [store index pred low-datom high-datom]
  (some #(when (pred %) %) (slice store index low-datom high-datom)))

(defn tail-filter
  [store index pred high-datom low-datom]
  (some #(when (pred %) %) (rslice store index high-datom low-datom)))

(defn size-filter
  [store index pred low-datom high-datom]
  (count (slice-filter store index pred low-datom high-datom)))

;; ── point lookups ────────────────────────────────────────────────────

(defn- search* [store e a v]
  (native/search (:handle store) e a v))

(defn fetch
  "[datom] if it exists in the store, else ()."
  [store datom]
  (->datoms (search* store (d/datom-e datom) (d/datom-a datom)
                     (d/datom-v datom))))

(defn e-datoms [store e] (->datoms (search* store e nil nil)))

(defn av-datoms [store a v] (->datoms (search* store nil a v)))

(defn v-datoms [store v] (->datoms (search* store nil nil v)))

(defn e-first-datom [store e] (first (e-datoms store e)))

(defn av-first-datom [store a v] (first (av-datoms store a v)))

(defn av-first-e [store a v]
  (some-> (av-first-datom store a v) d/datom-e))

(defn ea-first-datom [store e a]
  (first (->datoms (search* store e a nil))))

(defn ea-first-v [store e a]
  (some-> (ea-first-datom store e a) d/datom-v))

;; ── sizes ────────────────────────────────────────────────────────────

(defn datom-count [store index]
  (native/count-range (:handle store) (norm-index index) nil nil))

(defn e-size [store e] (native/count (:handle store) e nil nil))

(defn a-size [store a] (native/count (:handle store) nil a nil))

(defn av-size [store a v] (native/count (:handle store) nil a v))

(defn v-size [store v]
  (count (search* store nil nil v)))

(defn av-range-size
  ([store a lv hv] (av-range-size store a lv hv nil))
  ([store a lv hv _cap]
   (native/count-range (:handle store) :ave
                       [nil a (bound-v lv)]
                       [nil a (bound-v hv)])))

(defn cardinality
  "Number of distinct values of an attribute. Exact for small extents,
  the datom count otherwise (an upper bound; optimizer statistic only)."
  [store a]
  (let [n (long (a-size store a))]
    (if (<= n 16384)
      (count (into #{} (map d/datom-v)
                   (slice store :ave (d/datom c/e0 a c/v0)
                          (d/datom c/emax a c/vmax))))
      n)))

(defn default-ratio
  "Fan-out: size / cardinality."
  [store a]
  (let [card (long (cardinality store a))]
    (if (pos? card)
      (double (/ (long (a-size store a)) card))
      1.0)))

(defn init-max-eid [store] (native/max-eid (:handle store)))

;; ── writes ───────────────────────────────────────────────────────────

(defn load-datoms
  "Load datoms (positive = add, deleted = retract) into storage."
  [store datoms]
  (native/transact!
    (:handle store)
    (mapv (fn [datom]
            [(if (d/datom-added datom) :add :retract)
             (d/datom-e datom) (d/datom-a datom) (d/datom-v datom)])
          datoms))
  store)

;; ── value ranges (optimizer shapes) ──────────────────────────────────

(defn vpred
  [v]
  (cond
    (string? v)  (fn [x] (if (string? x) (= v x) false))
    (integer? v) (fn [x] (if (integer? x) (= (long v) (long x)) false))
    (keyword? v) (fn [x] (= v x))
    (nil? v)     (fn [x] (nil? x))
    :else        (fn [x] (= v x))))

(defn- range-bounds
  "[[lb-kind lv] [hb-kind hv]] -> [low-bound high-bound] native bound
  vectors for the ave index of ATTR (sentinels open the end)."
  [attr [[lk lv] [hk hv]]]
  [[nil attr (bound-v lv) (not= lk :open)]
   [nil attr (bound-v hv) (not= hk :open)]])

(defn- ave-range-datoms
  [store attr val-ranges]
  (let [ranges (or (seq val-ranges)
                   [[[:closed c/v0] [:closed c/vmax]]])]
    (into []
          (mapcat (fn [vrange]
                    (let [[lo hi] (range-bounds attr vrange)]
                      (native/slice (:handle store) :ave lo hi))))
          ranges)))

(defn ave-tuples-list
  "Tuples of [e] (or [e v] when GET-V?) for ATTR over VAL-RANGES,
  filtered by VPRED on the value."
  ([store attr val-ranges vpred get-v?]
   (ave-tuples-list store attr val-ranges vpred get-v? nil))
  ([store attr val-ranges vpred get-v? indices]
   (let [ds   (ave-range-datoms store attr val-ranges)
         keep (when indices (set indices))]
     (loop [ds (seq ds), i 0, res []]
       (if ds
         (let [[e _ v] (first ds)]
           (if (and (or (nil? vpred) (vpred v))
                    (or (nil? keep) (contains? keep i)))
             (recur (next ds) (inc i)
                    (conj res (if get-v?
                                (object-array [e v])
                                (object-array [e]))))
             (recur (next ds) (inc i) res)))
         res)))))

(defn ave-tuples
  "Emit ave tuples to OUT (a pipe or sink), ending with the end-scan
  sentinel, matching the upstream pipe contract."
  ([store out attr val-ranges vpred get-v?]
   (ave-tuples store out attr val-ranges vpred get-v? nil))
  ([store out attr val-ranges vpred get-v? indices]
   (p/add-batch out (ave-tuples-list store attr val-ranges vpred get-v?
                                     indices))
   (p/add-one out :datalevin/end-scan)
   out))

(defn sample-ave-tuples-list
  "Up to c/init-exec-size-threshold tuples reservoir-sampled from the
  MCOUNT-datom extent of ATTR over VAL-RANGES."
  [store attr mcount val-ranges vpred get-v?]
  (when mcount
    (let [indices (u/reservoir-sampling (long mcount)
                                        c/init-exec-size-threshold)]
      (ave-tuples-list store attr val-ranges vpred get-v? indices))))

(defn sample-ave-tuples
  [store out attr mcount val-ranges vpred get-v?]
  (when-some [tuples (sample-ave-tuples-list store attr mcount val-ranges
                                             vpred get-v?)]
    (p/add-batch out tuples)
    (p/add-one out :datalevin/end-scan)
    out))

(defn e-sample
  "Sampled [e] tuples across ATTR's full value range."
  [store a]
  (or (sample-ave-tuples-list store a (a-size store a) nil nil false) []))

;; ── scan pipeline (the optimizer's physical operators) ───────────────

(defn- tuple-attr-values
  "Values of (E, ATTR) surviving :pred and :fidx, or nil to reject."
  [store tuple e [attr {:keys [pred fidx]}]]
  (let [vs (into []
                 (comp (map (fn [[_ _ v]] v))
                       (filter (fn [v]
                                 (and (or (nil? pred) (pred v))
                                      (or (nil? fidx)
                                          (= v (aget tuple (int fidx))))))))
                 (search* store e attr nil))]
    (when (seq vs) vs)))

(defn eav-scan-v-list
  "For each in-tuple, merge the values of ATTRS-V ([[attr opts]...],
  opts :pred/:fidx/:skip?) for the eid at EID-IDX; tuples missing any
  attr drop, card-many attrs multiply, :skip? attrs only filter."
  [store in eid-idx attrs-v]
  (when (seq attrs-v)
    (let [sch     (schema store)
          attrs-v (sort-by #(get-in sch [(first %) :db/aid]) (vec attrs-v))]
      (loop [ts (seq in), res []]
        (if ts
          (let [tuple (first ts)
                e     (aget tuple (int eid-idx))
                cols  (loop [avs (seq attrs-v), acc []]
                        (if avs
                          (let [[attr m :as av] (first avs)]
                            (if-some [vs (tuple-attr-values
                                           store tuple e av)]
                              (recur (next avs)
                                     (if (:skip? m) acc (conj acc vs)))
                              :reject))
                          acc))]
            (if (identical? :reject cols)
              (recur (next ts) res)
              (if (empty? cols)
                (recur (next ts) (conj res tuple))
                (recur (next ts)
                       (into res
                             (r/prod-tuples (r/single-tuples tuple)
                                            (r/many-tuples cols)))))))
          res)))))

(defn eav-scan-v
  [store in out eid-idx attrs-v]
  (let [tuples (loop [t (p/produce in), acc []]
                 (if t (recur (p/produce in) (conj acc t)) acc))]
    (when (seq attrs-v)
      (p/add-batch out (eav-scan-v-list store tuples eid-idx attrs-v))
      (p/add-one out :datalevin/end-scan))
    out))

(defn eav-filter-presence-list
  [store in eid-idx attr]
  (eav-scan-v-list store in eid-idx [[attr {:skip? true}]]))

(defn val-eq-scan-e-list
  "Append, for each in-tuple, every e whose (ATTR, tuple[V-IDX]) datom
  exists; with BOUND, append BOUND when its datom exists."
  ([store in v-idx attr]
   (when attr
     (loop [ts (seq in), seen {}, res []]
       (if ts
         (let [tuple (first ts)
               v     (aget tuple (int v-idx))
               es    (if-some [hit (find seen v)]
                       (val hit)
                       (mapv first (search* store nil attr v)))]
           (recur (next ts)
                  (assoc seen v es)
                  (into res (map #(r/conj-tuple tuple %)) es)))
         res))))
  ([store in v-idx attr bound]
   (when attr
     (loop [ts (seq in), res []]
       (if ts
         (let [tuple (first ts)
               v     (aget tuple (int v-idx))]
           (recur (next ts)
                  (if (pos? (long (native/count (:handle store)
                                                bound attr v)))
                    (conj res (r/conj-tuple tuple (long bound)))
                    res)))
         res)))))

(defn val-eq-scan-e
  ([store in out v-idx attr]
   (let [tuples (loop [t (p/produce in), acc []]
                  (if t (recur (p/produce in) (conj acc t)) acc))]
     (when attr
       (p/add-batch out (val-eq-scan-e-list store tuples v-idx attr))
       (p/add-one out :datalevin/end-scan))
     out))
  ([store in out v-idx attr bound]
   (let [tuples (loop [t (p/produce in), acc []]
                  (if t (recur (p/produce in) (conj acc t)) acc))]
     (when attr
       (p/add-batch out (val-eq-scan-e-list store tuples v-idx attr bound))
       (p/add-one out :datalevin/end-scan))
     out)))

(defn val-eq-filter-e-list
  "Keep in-tuples whose (tuple[F-IDX], ATTR, tuple[V-IDX]) datom exists."
  [store in v-idx attr f-idx]
  (when attr
    (into []
          (filter (fn [tuple]
                    (pos? (long (native/count
                                  (:handle store)
                                  (aget tuple (int f-idx)) attr
                                  (aget tuple (int v-idx)))))))
          in)))

(defn val-eq-filter-e
  [store in out v-idx attr f-idx]
  (let [tuples (loop [t (p/produce in), acc []]
                 (if t (recur (p/produce in) (conj acc t)) acc))]
    (when attr
      (p/add-batch out (val-eq-filter-e-list store tuples v-idx attr f-idx))
      (p/add-one out :datalevin/end-scan))
    out))

;; ── search-tuples helpers (db's case tree) ───────────────────────────

(defn ea-tuples
  "Tuples of [v] for (E, A)."
  [store e a]
  (mapv (fn [[_ _ v]] (object-array [v])) (search* store e a nil)))

(defn ev-tuples
  "Tuples of [attr] for E's datoms whose value equals V."
  [store e v]
  (let [p (vpred v)]
    (into []
          (comp (filter (fn [[_ _ dv]] (p dv)))
                (map (fn [[_ a _]] (object-array [a]))))
          (search* store e nil nil))))

(defn e-tuples
  "Tuples of [attr v] for E."
  [store e]
  (mapv (fn [[_ a v]] (object-array [a v])) (search* store e nil nil)))

(defn av-tuples
  "Tuples of [e] for (A, V)."
  [store a v]
  (mapv (fn [[e _ _]] (object-array [e])) (search* store nil a v)))

(defn a-tuples
  "Tuples of [e v] for A."
  [store a]
  (ave-tuples-list store a nil nil true))

(defn v-tuples
  "Tuples of [e attr] for datoms whose value equals V."
  [store v]
  (let [p (vpred v)]
    (into []
          (comp (filter (fn [[_ _ dv]] (p dv)))
                (map (fn [[e a _]] (object-array [e a]))))
          (search* store nil nil nil))))

(defn all-tuples
  "Tuples of [e attr v] over the whole store."
  [store]
  (mapv (fn [[e a v]] (object-array [e a v])) (search* store nil nil nil)))

;; ── datalevin.interface bridge ───────────────────────────────────────
;; Vendored code (tx.common, query.cache, ...) reaches storage through
;; the interface protocols; the subset the query family uses delegates
;; to the fns above. Bodies are ns-qualified: a protocol method named
;; like the fn it calls would otherwise resolve the name to itself.

(extend-type Store
  i/IStore
  (opts [s] (datalevin.storage/opts s))
  (db-name [s] (datalevin.storage/db-name s))
  (dir [s] (datalevin.storage/dir s))
  (close [s] (datalevin.storage/close s))
  (closed? [s] (datalevin.storage/closed? s))
  (last-modified [s] (datalevin.storage/last-modified s))
  (max-tx [s] (datalevin.storage/max-tx s))
  (schema [s] (datalevin.storage/schema s))
  (rschema [s] (datalevin.storage/rschema s))
  (attrs [s] (datalevin.storage/attrs s))
  (init-max-eid [s] (datalevin.storage/init-max-eid s))
  (datom-count [s index] (datalevin.storage/datom-count s index))
  (load-datoms [s datoms] (datalevin.storage/load-datoms s datoms))
  (fetch [s datom] (datalevin.storage/fetch s datom))
  (populated? [s index low-datom high-datom]
    (datalevin.storage/populated? s index low-datom high-datom))
  (size [s index low-datom high-datom]
    (datalevin.storage/size s index low-datom high-datom))
  (e-size [s e] (datalevin.storage/e-size s e))
  (a-size [s a] (datalevin.storage/a-size s a))
  (e-sample [s a] (datalevin.storage/e-sample s a))
  (default-ratio [s a] (datalevin.storage/default-ratio s a))
  (v-size [s v] (datalevin.storage/v-size s v))
  (av-size [s a v] (datalevin.storage/av-size s a v))
  (av-range-size [s a lv hv] (datalevin.storage/av-range-size s a lv hv))
  (cardinality [s a] (datalevin.storage/cardinality s a))
  (head [s index low-datom high-datom]
    (datalevin.storage/head s index low-datom high-datom))
  (tail [s index high-datom low-datom]
    (datalevin.storage/tail s index high-datom low-datom))
  (slice
    ([s index low-datom high-datom]
     (datalevin.storage/slice s index low-datom high-datom))
    ([s index low-datom high-datom n]
     (datalevin.storage/slice s index low-datom high-datom n)))
  (rslice
    ([s index high-datom low-datom]
     (datalevin.storage/rslice s index high-datom low-datom))
    ([s index high-datom low-datom n]
     (datalevin.storage/rslice s index high-datom low-datom n)))
  (e-datoms [s e] (datalevin.storage/e-datoms s e))
  (e-first-datom [s e] (datalevin.storage/e-first-datom s e))
  (av-datoms [s a v] (datalevin.storage/av-datoms s a v))
  (av-first-datom [s a v] (datalevin.storage/av-first-datom s a v))
  (ea-first-datom [s e a] (datalevin.storage/ea-first-datom s e a))
  (ea-first-v [s e a] (datalevin.storage/ea-first-v s e a))
  (av-first-e [s a v] (datalevin.storage/av-first-e s a v))
  (v-datoms [s v] (datalevin.storage/v-datoms s v))
  (size-filter [s index pred low-datom high-datom]
    (datalevin.storage/size-filter s index pred low-datom high-datom))
  (head-filter [s index pred low-datom high-datom]
    (datalevin.storage/head-filter s index pred low-datom high-datom))
  (tail-filter [s index pred high-datom low-datom]
    (datalevin.storage/tail-filter s index pred high-datom low-datom))
  (slice-filter [s index pred low-datom high-datom]
    (datalevin.storage/slice-filter s index pred low-datom high-datom))
  (rslice-filter [s index pred high-datom low-datom]
    (datalevin.storage/rslice-filter s index pred high-datom low-datom))
  (ave-tuples
    ([s out attr val-ranges]
     (datalevin.storage/ave-tuples s out attr val-ranges nil false))
    ([s out attr val-ranges vpred]
     (datalevin.storage/ave-tuples s out attr val-ranges vpred false))
    ([s out attr val-ranges vpred get-v?]
     (datalevin.storage/ave-tuples s out attr val-ranges vpred get-v?))
    ([s out attr val-ranges vpred get-v? indices]
     (datalevin.storage/ave-tuples s out attr val-ranges vpred get-v?
                                   indices)))
  (ave-tuples-list [s attr val-ranges vpred get-v?]
    (datalevin.storage/ave-tuples-list s attr val-ranges vpred get-v?))
  (sample-ave-tuples [s out attr mcount val-ranges vpred get-v?]
    (datalevin.storage/sample-ave-tuples s out attr mcount val-ranges
                                         vpred get-v?))
  (sample-ave-tuples-list [s attr mcount val-ranges vpred get-v?]
    (datalevin.storage/sample-ave-tuples-list s attr mcount val-ranges
                                              vpred get-v?))
  (eav-scan-v [s in out eid-idx attrs-v]
    (datalevin.storage/eav-scan-v s in out eid-idx attrs-v))
  (eav-scan-v-list [s in eid-idx attrs-v]
    (datalevin.storage/eav-scan-v-list s in eid-idx attrs-v))
  (val-eq-scan-e
    ([s in out v-idx attr]
     (datalevin.storage/val-eq-scan-e s in out v-idx attr))
    ([s in out v-idx attr bound]
     (datalevin.storage/val-eq-scan-e s in out v-idx attr bound)))
  (val-eq-scan-e-list
    ([s in v-idx attr]
     (datalevin.storage/val-eq-scan-e-list s in v-idx attr))
    ([s in v-idx attr bound]
     (datalevin.storage/val-eq-scan-e-list s in v-idx attr bound)))
  (val-eq-filter-e [s in out v-idx attr f-idx]
    (datalevin.storage/val-eq-filter-e s in out v-idx attr f-idx))
  (val-eq-filter-e-list [s in v-idx attr f-idx]
    (datalevin.storage/val-eq-filter-e-list s in v-idx attr f-idx)))
