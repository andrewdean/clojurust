;;
;; cljrs replacement for datalevin.db (upstream is 1.7k lines mixing
;; the query-path DB abstraction with the JVM tx pipeline, remote
;; stores, and result caches). This keeps the query-path surface: the
;; protocols, the DB record over datalevin.storage, and the schema /
;; entid helpers. Protocol shapes, case trees, and helper semantics
;; follow upstream datalevin 78b199e8 db.clj (EPL-2.0, Copyright (c)
;; Nikita Prokopov, Huahai Yang). Not ported: the datalevin tx
;; pipeline (writes go through storage/load-datoms), pending-tx
;; caches (always empty), result caches (wrap-cache is the identity),
;; remote/client stores, and UDF ensures. Loaded on :cljrsh only.
;;
(ns ^:no-doc datalevin.db
  "Datalog DB abstraction"
  (:refer-clojure :exclude [update assoc sync])
  (:require
   [datalevin.constants :as c :refer [e0 emax v0 vmax]]
   [datalevin.datom :as d :refer [datom datom?]]
   [datalevin.db.tx.common :as txcommon]
   [datalevin.inline :refer [update assoc]]
   [datalevin.relation :as rel]
   [datalevin.storage :as s]
   [datalevin.util :as u :refer [case-tree raise]]
   [datalevin.validate :as vld]))

;; ── protocols (verbatim from upstream) ───────────────────────────────

(defprotocol ISearch
  (-search [db pattern])
  (-search-tuples [db pattern])
  (-count [db pattern] [data pattern cap])
  (-first [db pattern]))

(defprotocol IIndexAccess
  (-populated? [db index c1 c2 c3])
  (-datoms [db index] [db index c1] [db index c1 c2] [db index c1 c2 c3]
    [db index c1 c2 c3 n])
  (-e-datoms [db e])
  (-av-datoms [db attr v])
  (-range-datoms [db index start-datom end-datom])
  (-seek-datoms [db index c1 c2 c3] [db index c1 c2 c3 n])
  (-rseek-datoms [db index c1 c2 c3] [db index c1 c2 c3 n])
  (-cardinality [db attr])
  (-index-range [db attr start end])
  (-index-range-size [db attr start end]))

(defprotocol IDB
  (-schema [db])
  (-rschema [db])
  (-attrs-by [db property])
  (-is-attr? [db attr property])
  (-clear-tx-cache [db]))

(defprotocol ISearchable (-searchable? [_]))

(extend-type Object ISearchable (-searchable? [_] false))
(extend-type nil ISearchable (-searchable? [_] false))

(defprotocol ITuples
  (-init-tuples [db out a v-range pred get-v?])
  (-init-tuples-list [db a v-range pred get-v?])
  (-sample-init-tuples [db out a mcount v-range pred get-v?])
  (-sample-init-tuples-list [db a mcount v-range pred get-v?])
  (-e-sample [db a])
  (-default-ratio [db a])
  (-eav-scan-v [db in out eid-idx attrs-v])
  (-eav-scan-v-list [db in eid-idx attrs-v])
  (-eav-filter-presence-list [db in eid-idx attr])
  (-val-eq-scan-e [db in out v-idx attr] [db in out v-idx attr bound])
  (-val-eq-scan-e-list [db in v-idx attr] [db in v-idx attr bound])
  (-val-eq-filter-e [db in out v-idx attr f-idx])
  (-val-eq-filter-e-list [db in v-idx attr f-idx]))

;; ----------------------------------------------------------------------------

(declare resolve-datom components->pattern components->end-datom)

(defrecord TxReport [db-before db-after tx-data tempids tx-meta])

;; Pending-tx caches are not ported: queries always read committed
;; storage state.
(defn ^:no-doc pending-tx-cache? [_db] false)

(defrecord DB [store max-eid max-tx eavt avet pull-patterns]

  ISearchable
  (-searchable? [_] true)

  IDB
  (-schema [_] (s/schema store))
  (-rschema [_] (s/rschema store))
  (-attrs-by [db property] ((-rschema db) property))
  (-is-attr? [db attr property] (contains? (-attrs-by db property) attr))
  (-clear-tx-cache [db] db)

  ITuples
  (-init-tuples
    [db out a v-ranges pred get-v?]
    (s/ave-tuples store out a v-ranges pred get-v?))

  (-init-tuples-list
    [db a v-ranges pred get-v?]
    (s/ave-tuples-list store a v-ranges pred get-v?))

  (-sample-init-tuples
    [db out a mcount v-ranges pred get-v?]
    (s/sample-ave-tuples store out a mcount v-ranges pred get-v?))

  (-sample-init-tuples-list
    [db a mcount v-ranges pred get-v?]
    (s/sample-ave-tuples-list store a mcount v-ranges pred get-v?))

  (-e-sample [db a] (s/e-sample store a))

  (-default-ratio [db a] (s/default-ratio store a))

  (-eav-scan-v
    [db in out eid-idx attrs-v]
    (s/eav-scan-v store in out eid-idx attrs-v))

  (-eav-scan-v-list
    [db in eid-idx attrs-v]
    (s/eav-scan-v-list store in eid-idx attrs-v))

  (-eav-filter-presence-list
    [db in eid-idx attr]
    (s/eav-scan-v-list store in eid-idx [[attr {:skip? true}]]))

  (-val-eq-scan-e
    ([db in out v-idx attr]
     (s/val-eq-scan-e store in out v-idx attr))
    ([db in out v-idx attr bound]
     (s/val-eq-scan-e store in out v-idx attr bound)))

  (-val-eq-scan-e-list
    ([db in v-idx attr]
     (s/val-eq-scan-e-list store in v-idx attr))
    ([db in v-idx attr bound]
     (s/val-eq-scan-e-list store in v-idx attr bound)))

  (-val-eq-filter-e
    [db in out v-idx attr f-idx]
    (s/val-eq-filter-e store in out v-idx attr f-idx))

  (-val-eq-filter-e-list
    [db in v-idx attr f-idx]
    (s/val-eq-filter-e-list store in v-idx attr f-idx))

  ISearch
  (-search
    [db pattern]
    (let [[e a v _] pattern]
      (case-tree
        [e a (some? v)]
        [(s/fetch store (datom e a v)) ; e a v
         (s/slice store :eav (datom e a c/v0) (datom e a c/vmax)) ; e a _
         (s/slice-filter store :eav
                         (fn [d] (when ((s/vpred v) (d/datom-v d)) d))
                         (datom e nil nil)
                         (datom e nil nil))  ; e _ v
         (s/e-datoms store e) ; e _ _
         (s/av-datoms store a v) ; _ a v
         (mapv #(datom (aget ^objects % 0) a (aget ^objects % 1))
               (s/ave-tuples-list
                 store a [[[:closed c/v0] [:closed c/vmax]]] nil true)) ; _ a _
         (s/slice-filter store :eav
                         (fn [d] (when ((s/vpred v) (d/datom-v d)) d))
                         (datom e0 nil nil)
                         (datom emax nil nil)) ; _ _ v
         (s/slice store :eav (datom e0 nil nil) (datom emax nil nil))]))) ; _ _ _

  (-search-tuples
    [db pattern]
    (let [[e a v _] pattern]
      (case-tree
        [e a (some? v)]
        [(when (s/populated? store :eav (d/datom e a v) (d/datom e a v))
           (rel/single-tuples (object-array [e a v]))) ; e a v
         (s/ea-tuples store e a) ; e a _
         (s/ev-tuples store e v)  ; e _ v
         (s/e-tuples store e) ; e _ _
         (s/av-tuples store a v) ; _ a v
         (s/a-tuples store a) ; _ a _
         (s/v-tuples store v) ; _ _ v
         (s/all-tuples store)]))) ; _ _ _

  (-first
    [db pattern]
    (let [[e a v _] pattern]
      (case-tree
        [e a (some? v)]
        [(first (s/fetch store (datom e a v))) ; e a v
         (s/ea-first-datom store e a) ; e a _
         (s/head-filter store :eav
                        (fn [d]
                          (when ((s/vpred v) (d/datom-v d)) d))
                        (datom e nil nil)
                        (datom e nil nil))  ; e _ v
         (s/e-first-datom store e) ; e _ _
         (s/av-first-datom store a v) ; _ a v
         (s/head store :ave (datom e0 a nil) (datom emax a nil)) ; _ a _
         (s/head-filter store :eav
                        (fn [d]
                          (when ((s/vpred v) (d/datom-v d)) d))
                        (datom e0 nil nil)
                        (datom emax nil nil)) ; _ _ v
         (s/head store :eav (datom e0 nil nil) (datom emax nil nil))]))) ; _ _ _

  (-count
    ([db pattern]
     (-count db pattern nil))
    ([db pattern cap]
     (let [[e a v] pattern]
       (case-tree
         [e a (some? v)]
         [(s/size store :eav (datom e a v) (datom e a v)) ; e a v
          (s/size store :eav (datom e a c/v0) (datom e a c/vmax)) ; e a _
          (s/size-filter store :eav
                         (fn [d] ((s/vpred v) (d/datom-v d)))
                         (datom e nil nil) (datom e nil nil))  ; e _ v
          (s/e-size store e) ; e _ _
          (s/av-size store a v) ; _ a v
          (s/a-size store a) ; _ a _
          (s/v-size store v) ; _ _ v, for ref only
          (s/datom-count store :eav)])))) ; _ _ _

  IIndexAccess
  (-populated?
    [db index c1 c2 c3]
    (s/populated? store index
                  (components->pattern db index c1 c2 c3 e0 v0)
                  (components->pattern db index c1 c2 c3 emax vmax)))

  (-datoms
    ([db index]
     (-datoms db index nil nil nil))
    ([db index c1]
     (-datoms db index c1 nil nil))
    ([db index c1 c2]
     (-datoms db index c1 c2 nil))
    ([db index c1 c2 c3]
     (s/slice store index
              (components->pattern db index c1 c2 c3 e0 v0)
              (components->pattern db index c1 c2 c3 emax vmax)))
    ([db index c1 c2 c3 n]
     (s/slice store index
              (components->pattern db index c1 c2 c3 e0 v0)
              (components->pattern db index c1 c2 c3 emax vmax)
              n)))

  (-e-datoms [db e] (s/e-datoms store e))

  (-av-datoms [db attr v] (s/av-datoms store attr v))

  (-range-datoms
    [db index start-datom end-datom]
    (s/slice store index start-datom end-datom))

  (-seek-datoms
    ([db index c1 c2 c3]
     (s/slice store index
              (components->pattern db index c1 c2 c3 e0 v0)
              (components->end-datom db index c1 c2 c3 emax vmax)))
    ([db index c1 c2 c3 n]
     (s/slice store index
              (components->pattern db index c1 c2 c3 e0 v0)
              (components->end-datom db index c1 c2 c3 emax vmax)
              n)))

  (-rseek-datoms
    ([db index c1 c2 c3]
     (s/rslice store index
               (components->pattern db index c1 c2 c3 emax vmax)
               (components->end-datom db index c1 c2 c3 e0 v0)))
    ([db index c1 c2 c3 n]
     (s/rslice store index
               (components->pattern db index c1 c2 c3 emax vmax)
               (components->end-datom db index c1 c2 c3 e0 v0)
               n)))

  (-cardinality
    [db attr]
    (s/cardinality store attr))

  (-index-range
    [db attr start end]
    (do (vld/validate-attr attr (list '-index-range 'db attr start end))
        (s/slice store :ave (resolve-datom db nil attr start e0 v0)
                 (resolve-datom db nil attr end emax vmax))))

  (-index-range-size
    [db attr start end]
    (s/av-range-size store attr start end)))

(defn ^:no-doc -ea-populated?
  "Test whether an entity has an attribute without caching or retrieving its
   value. Intended for batched existence probes with mostly unique entities."
  [db e a]
  (s/populated? (:store db) :eav
                (d/datom e a c/v0)
                (d/datom e a c/vmax)))

(defn db?
  "Check if x is an instance of DB."
  [x]
  (boolean (-searchable? x)))

(defn search-datoms [db e a v] (-search db [e a v]))

(defn count-datoms [db e a v] (-count db [e a v] nil))

(defn seek-datoms
  ([db index]
   (-seek-datoms db index nil nil nil))
  ([db index c1]
   (-seek-datoms db index c1 nil nil))
  ([db index c1 c2]
   (-seek-datoms db index c1 c2 nil))
  ([db index c1 c2 c3]
   (-seek-datoms db index c1 c2 c3))
  ([db index c1 c2 c3 n]
   (-seek-datoms db index c1 c2 c3 n)))

(defn rseek-datoms
  ([db index]
   (-rseek-datoms db index nil nil nil))
  ([db index c1]
   (-rseek-datoms db index c1 nil nil))
  ([db index c1 c2]
   (-rseek-datoms db index c1 c2 nil))
  ([db index c1 c2 c3]
   (-rseek-datoms db index c1 c2 c3))
  ([db index c1 c2 c3 n]
   (-rseek-datoms db index c1 c2 c3 n)))

(defn max-eid [db] (s/init-max-eid (:store db)))

;; ── constructors ─────────────────────────────────────────────────────

(defn new-db
  "Wrap an open datalevin.storage store in a DB."
  [store]
  (->DB store (s/init-max-eid store) 0 nil nil nil))

(defn- apply-schema!
  "Translate a datalevin schema map ({attr {:db/cardinality ...,
  :db/valueType ...}}) onto the store's attribute properties."
  [store schema]
  (doseq [[attr props] schema]
    (s/set-attr! store attr
                 {:cardinality-many (= :db.cardinality/many
                                       (:db/cardinality props))
                  :ref (= :db.type/ref (:db/valueType props))}))
  store)

(defn empty-db
  "Open (creating if needed) a DB at DIR, optionally applying SCHEMA."
  ([dir] (empty-db dir nil))
  ([dir schema] (empty-db dir schema nil))
  ([dir schema opts]
   (let [store (s/open dir (or opts {}))]
     (when schema (apply-schema! store schema))
     (new-db store))))

(defn init-db
  "Open a DB at DIR and load DATOMS into it."
  ([datoms dir] (init-db datoms dir nil nil))
  ([datoms dir schema] (init-db datoms dir schema nil))
  ([datoms dir schema opts]
   (let [db (empty-db dir schema opts)]
     (s/load-datoms (:store db) datoms)
     (new-db (:store db)))))

(defn close-db [db]
  (s/close (:store db))
  nil)

;; ── datom resolution ─────────────────────────────────────────────────

(defn multival?
  [db attr]
  (txcommon/multival? db attr))

(defn multi-value?
  [db attr value]
  (txcommon/multi-value? db attr value))

(defn ref?
  [db attr]
  (txcommon/ref? db attr))

(defn component?
  [db attr]
  (txcommon/component? db attr))

(defn tuple-attr?
  [db attr]
  (txcommon/tuple-attr? db attr))

(defn tuple-type?
  [db attr]
  (txcommon/tuple-type? db attr))

(defn tuple-types?
  [db attr]
  (txcommon/tuple-types? db attr))

(defn tuple-source?
  [db attr]
  (txcommon/tuple-source? db attr))

(defn entid
  [db eid]
  (txcommon/entid db eid))

(defn entid-strict
  [db eid]
  (txcommon/entid-strict db eid))

(defn entid-some
  [db eid]
  (txcommon/entid-some db eid))

(defn reverse-ref?
  [attr]
  (txcommon/reverse-ref? attr))

(defn reverse-ref
  [attr]
  (txcommon/reverse-ref attr))

(defn- resolve-datom
  [db e a v default-e default-v]
  (when a (vld/validate-attr a (list 'resolve-datom 'db e a v default-e
                                    default-v)))
  (let [v? (some? v)]
    (datom
      (or (entid-some db e) default-e)  ;; e
      a                                 ;; a
      (if (and v? (ref? db a))          ;; v
        (entid-strict db v)
        (if v? v default-v)))))

(defn- components->pattern
  [db index c0 c1 c2 default-e default-v]
  (case index
    (:eav :eavt) (resolve-datom db c0 c1 c2 default-e default-v)
    (:ave :avet) (resolve-datom db c2 c0 c1 default-e default-v)))

(defn- components->end-datom
  [_ index c0 c1 _ default-e default-v]
  (datom default-e
         (case index
           (:eav :eavt) c1
           (:ave :avet) c0)
         default-v))
