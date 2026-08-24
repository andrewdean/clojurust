;;
;; Copyright (c) Huahai Yang. All rights reserved.
;; The use and distribution terms for this software are covered by the
;; Eclipse Public License 2.0 (https://opensource.org/license/epl-2-0)
;; which can be found in the file LICENSE at the root of this distribution.
;; By using this software in any fashion, you are agreeing to be bound by
;; the terms of this license.
;; You must not remove this notice, or any other, from this software.
;;
;; cljrs port note: the :clj arm's pipes exist for producer/consumer
;; threading (LinkedBlockingQueue + Semaphore). cljrs evaluation is
;; single-threaded, so the :cljrsh arm keeps the same public surface
;; (pipe?, finish, produce, add-batch, drain-to, reset, total, and the
;; constructors) over volatile state, and producing from an unfinished
;; empty pipe raises instead of blocking. Non-pipe sinks are volatiles
;; of persistent vectors, created with new-sink.
(ns ^:no-doc datalevin.pipe
  "Tuple pipes for query execution"
  (:refer-clojure :exclude [update assoc])
  (:require
   [datalevin.constants :as c]
   [datalevin.timeout :as timeout]
   [datalevin.util :as u])
  #?(:cljrsh nil
     :clj (:import
           [java.util List Collection HashMap]
           [java.util.concurrent LinkedBlockingQueue Semaphore TimeUnit]
           [org.eclipse.collections.impl.list.mutable FastList])))

#?(:cljrsh
   (do

(defn new-sink
  "A mutable tuple sink: a volatile of a persistent vector."
  []
  (volatile! []))

(defn sink-seq
  "The tuples currently in a non-pipe sink."
  [sink]
  @sink)

(defprotocol ITuplePipe
  (-finish [this])
  (-produce [this])
  (-add-batch [this tuples])
  (-reset [this])
  (-total [this]))

(deftype TuplePipe [state]
  ITuplePipe
  (-finish [_]
    (vswap! state clojure.core/assoc :finished true))
  (-produce [_]
    (timeout/assert-time-left)
    (let [{:keys [tuples i finished]} @state]
      (if (< i (count tuples))
        (let [t (nth tuples i)]
          (vswap! state clojure.core/assoc :i (inc i))
          (if (identical? :datalevin/end-scan t)
            (do (vswap! state clojure.core/assoc :finished true)
                nil)
            t))
        (when-not finished
          (u/raise "Producing from an unfinished pipe in single-threaded mode"
                   {})))))
  (-add-batch [_ ts]
    (vswap! state clojure.core/update :tuples into ts)
    true)
  (-reset [_]
    (vreset! state {:tuples [] :i 0 :finished false}))
  (-total [_] 0))

(deftype CountedTuplePipe [state]
  ITuplePipe
  (-finish [_]
    (vswap! state clojure.core/assoc :finished true))
  (-produce [_]
    (timeout/assert-time-left)
    (let [{:keys [tuples i finished]} @state]
      (if (< i (count tuples))
        (let [t (nth tuples i)]
          (if (identical? :datalevin/end-scan t)
            (do (vswap! state #(-> %
                                   (clojure.core/assoc :i (inc i))
                                   (clojure.core/assoc :finished true)))
                nil)
            (do (vswap! state #(-> %
                                   (clojure.core/assoc :i (inc i))
                                   (clojure.core/update :total inc)))
                t)))
        (when-not finished
          (u/raise "Producing from an unfinished pipe in single-threaded mode"
                   {})))))
  (-add-batch [_ ts]
    (vswap! state clojure.core/update :tuples into ts)
    true)
  (-reset [_]
    (vreset! state {:tuples [] :i 0 :finished false :total 0}))
  (-total [_] (:total @state)))

(deftype ListTuplePipe [tuples ipos]
  ITuplePipe
  (-finish [_] nil)
  (-produce [_]
    (let [i @ipos]
      (when (< i (count tuples))
        (when (zero? (rem i (max 1 (long c/query-pipe-batch-size))))
          (timeout/assert-time-left))
        (let [tuple (nth tuples i)]
          (vreset! ipos (inc i))
          tuple))))
  (-add-batch [_ _]
    (u/raise "Cannot add tuples to a list input pipe" {}))
  (-reset [_] (vreset! ipos 0))
  (-total [_] 0))

(deftype OrJoinTuplePipe [tuples bound-idx or-by-bound free-var-idx
                          tuple-len state]
  ITuplePipe
  (-finish [_] nil)
  (-produce [_]
    (loop []
      (let [{:keys [i current matches j]} @state]
        (if (and matches (< j (count matches)))
          (let [or-tuple (nth matches j)
                fv       (aget or-tuple free-var-idx)
                joined   (object-array (inc tuple-len))]
            (System/arraycopy current 0 joined 0 tuple-len)
            (aset joined tuple-len fv)
            (vswap! state clojure.core/assoc :j (inc j))
            joined)
          (when (< i (count tuples))
            (let [in-tuple (nth tuples i)
                  bv       (aget in-tuple bound-idx)
                  m        (get or-by-bound bv)]
              (if (and m (pos? (count m)))
                (vreset! state {:i (inc i) :current in-tuple
                                :matches m :j 0})
                (vreset! state {:i (inc i) :current nil
                                :matches nil :j 0}))
              (recur)))))))
  (-add-batch [_ _]
    (u/raise "Cannot add tuples to an or-join input pipe" {}))
  (-reset [_]
    (vreset! state {:i 0 :current nil :matches nil :j 0}))
  (-total [_] 0))

(defn pipe?
  [x]
  (or (instance? TuplePipe x)
      (instance? CountedTuplePipe x)
      (instance? ListTuplePipe x)
      (instance? OrJoinTuplePipe x)))

(defn finish [pipe] (-finish pipe))

(defn produce [pipe] (-produce pipe))

(defn add-batch
  "Into a pipe, or into a non-pipe volatile sink; nil sink ignores."
  [x tuples]
  (cond
    (pipe? x) (-add-batch x tuples)
    (nil? x)  false
    :else     (do (vswap! x into tuples) true)))

(defn add-one
  "Add a single tuple to a pipe or a non-pipe volatile sink."
  [x tuple]
  (cond
    (pipe? x) (-add-batch x [tuple])
    (nil? x)  false
    :else     (do (vswap! x conj tuple) true)))

(defn drain-to
  [pipe sink]
  (loop [tuple (produce pipe)]
    (when tuple
      (add-one sink tuple)
      (recur (produce pipe)))))

(defn reset [pipe] (-reset pipe))

(defn total [pipe] (-total pipe))

(defn batch-buffer
  "A fresh batching buffer (no thread-local reuse on cljrs)."
  []
  (new-sink))

(defn tuple-pipe
  []
  (->TuplePipe (volatile! {:tuples [] :i 0 :finished false})))

(defn counted-tuple-pipe
  []
  (->CountedTuplePipe (volatile! {:tuples [] :i 0 :finished false :total 0})))

(defn list-tuple-pipe
  [tuples]
  (->ListTuplePipe (vec tuples) (volatile! 0)))

(defn remove-end-scan
  [tuples]
  (if (and (vector? tuples)
           (pos? (count tuples))
           (identical? :datalevin/end-scan (peek tuples)))
    (recur (pop tuples))
    tuples))

(defn or-join-tuple-pipe
  [tuples bound-idx or-by-bound free-var-idx tuple-len]
  (->OrJoinTuplePipe (vec tuples) bound-idx or-by-bound free-var-idx
                     tuple-len
                     (volatile! {:i 0 :current nil :matches nil :j 0})))

) ;; end :cljrsh arm

:clj
(do

(def ^:private ^ThreadLocal batch-buffer-tl
  (ThreadLocal.))

(defn batch-buffer
  "Returns a pre-allocated, thread-local FastList for batching.
   The buffer is cleared before returning. Caller should not hold
   references to it across batch operations."
  ^FastList []
  (let [^FastList buf (.get batch-buffer-tl)]
    (if buf
      (do (.clear buf) buf)
      (let [buf (FastList. (int c/query-pipe-batch-size))]
        (.set batch-buffer-tl buf)
        buf))))

(defn new-sink
  "A mutable tuple sink."
  []
  (FastList.))

(defn sink-seq
  "The tuples currently in a non-pipe sink."
  [sink]
  (seq sink))

(defn- enqueue
  [^LinkedBlockingQueue queue o]
  (try
    (.put queue o) ;; block when full
    true
    (catch InterruptedException e
      (.interrupt (Thread/currentThread))
      (u/raise "Interrupted while enqueuing to pipe" e {:object o}))))

(deftype TupleBatch [^List tuples ^long start ^long end])

(defn- acquire-permits
  [^Semaphore permits ^long n]
  (try
    (.acquire permits (int n))
    (catch InterruptedException e
      (.interrupt (Thread/currentThread))
      (u/raise "Interrupted while enqueuing to pipe" e {:tuple-count n}))))

(defn- enqueue-batches
  [^LinkedBlockingQueue queue ^Semaphore permits ^List tuples ^long batch-size]
  (let [n (.size tuples)]
    (loop [start 0]
      (when (< start n)
        (let [end (min n (+ start batch-size))
              cnt (- end start)]
          (acquire-permits permits cnt)
          (try
            (enqueue queue (TupleBatch. tuples start end))
            (catch Throwable e
              (.release permits (int cnt))
              (throw e)))
          (recur end))))))

(defn- release-batch
  [^Semaphore permits o]
  (when (instance? TupleBatch o)
    (let [^TupleBatch batch o]
      (.release permits (int (- (.-end batch) (.-start batch)))))))

(defprotocol IBatchedQueue
  (-flush [this])
  (-add [this tuple])
  (-add-all [this tuples])
  (-finish [this])
  (-produce [this])
  (-reset [this]))

(deftype BatchedQueue [^LinkedBlockingQueue queue
                       ^Semaphore permits
                       ^long batch-size
                       ^:unsynchronized-mutable ^FastList producer
                       ^:unsynchronized-mutable ^TupleBatch consumer
                       ^:unsynchronized-mutable ^long consumer-idx]
  IBatchedQueue
  (-flush [_]
    (timeout/assert-time-left)
    (when (pos? (.size producer))
      (enqueue-batches queue permits producer batch-size)
      (set! producer (FastList. (int batch-size)))))
  (-add [this tuple]
    (.add producer tuple)
    (when (>= (.size producer) batch-size)
      (-flush this))
    true)
  (-add-all [this tuples]
    (-flush this)
    (when (pos? (.size ^List tuples))
      (enqueue-batches queue permits tuples batch-size))
    true)
  (-finish [this]
    (-flush this)
    (enqueue queue :datalevin/end-scan))
  (-produce [_]
    (timeout/assert-time-left)
    (loop []
      (if consumer
        (let [i      consumer-idx
              end    (.-end consumer)
              tuple  (.get ^List (.-tuples consumer) i)
              next-i (inc i)]
          (if (= next-i end)
            (do (.release permits
                          (int (- end (.-start consumer))))
                (set! consumer nil)
                (set! consumer-idx 0))
            (set! consumer-idx next-i))
          tuple)
        (let [remaining (timeout/time-left)
              wait-ms   (if remaining
                          (max 1 (min (long c/query-pipe-timeout)
                                      (long remaining)))
                          (long c/query-pipe-timeout))
              o (.poll queue wait-ms
                       TimeUnit/MILLISECONDS)]
          (when (nil? o)
            (timeout/assert-time-left)
            (u/raise "Pipe take timed out waiting for producer"
                     {:timeout wait-ms}))
          (when-not (identical? :datalevin/end-scan o)
            (set! consumer o)
            (set! consumer-idx (.-start consumer))
            (recur))))))
  (-reset [_]
    (.clear producer)
    (when consumer
      (release-batch permits consumer)
      (set! consumer nil)
      (set! consumer-idx 0))
    (loop []
      (when-let [o (.poll queue)]
        (release-batch permits o)
        (recur)))))

(defn- batched-queue
  []
  (let [capacity   (long c/query-pipe-capacity)
        batch-size (Math/max 1 (Math/min capacity
                                         (long c/query-pipe-batch-size)))]
    (BatchedQueue. (LinkedBlockingQueue. capacity)
                   (Semaphore. (int capacity))
                   batch-size
                   (FastList. (int batch-size))
                   nil 0)))

(defprotocol ITuplePipe
  (pipe? [this] "test if implements this protocol")
  (finish [this] "send a sentinel to indicate end of this pipe")
  (produce [this]
    "take a tuple from the pipe, block if there is nothing to take (up to
     c/query-pipe-timeout), if encounter :datalevin/end-scan, return nil")
  (add-batch [this tuples]
    "Add a tuple batch without copying. The caller must not mutate it while the
     pipe is consuming it.")
  (drain-to [this sink] "pour all remaining content into sink")
  (reset [this] "reset the pipe for next round of operation")
  (total [this] "return the total number of tuples pass through the pipe"))

(defn add-one
  "Add a single tuple to a pipe or a collection sink."
  [x tuple]
  (if (nil? x)
    false
    (.add ^Collection x tuple)))

(extend-type Object
  ITuplePipe
  (pipe? [_] false)
  (add-batch [this tuples] (.addAll ^Collection this ^Collection tuples)))

(extend-type nil
  ITuplePipe
  (pipe? [_] false)
  (add-batch [_ _] false))

(deftype TuplePipe [^BatchedQueue state]
  ITuplePipe
  (pipe? [_] true)
  (finish [_] (-finish state))
  (produce [_] (-produce state))
  (add-batch [_ tuples] (-add-all state tuples))
  (drain-to [this sink]
    (loop [tuple (produce this)]
      (when tuple
        (.add ^Collection sink tuple)
        (recur (produce this)))))
  (reset [_] (-reset state))
  (total [_] 0)

  Collection
  (add [_ o] (-add state o))
  (addAll [_ l]
    (-add-all state (FastList. ^Collection l))))

(deftype CountedTuplePipe [^BatchedQueue state
                           ^:unsynchronized-mutable ^long total]
  ITuplePipe
  (pipe? [_] true)
  (finish [_] (-finish state))
  (produce [_]
    (let [o (-produce state)]
      (when o
        (set! total (u/long-inc total))
        o)))
  (add-batch [_ tuples] (-add-all state tuples))
  (drain-to [this sink]
    (loop [tuple (produce this)]
      (when tuple
        (.add ^Collection sink tuple)
        (recur (produce this)))))
  (reset [_] (-reset state))
  (total [_] total)

  Collection
  (add [_ o] (-add state o))
  (addAll [_ o]
    (-add-all state (FastList. ^Collection o))))

(defn tuple-pipe
  []
  (->TuplePipe (batched-queue)))

(defn counted-tuple-pipe
  []
  (->CountedTuplePipe (batched-queue) 0))

(deftype ListTuplePipe [^List tuples
                        ^:unsynchronized-mutable ^long i]
  ITuplePipe
  (pipe? [_] true)
  (finish [_] nil)
  (produce [_]
    (when (< i (.size tuples))
      (when (zero? (rem i (max 1 (long c/query-pipe-batch-size))))
        (timeout/assert-time-left))
      (let [tuple (.get tuples i)]
        (set! i (inc i))
        tuple)))
  (add-batch [_ _]
    (u/raise "Cannot add tuples to a list input pipe" {}))
  (drain-to [this sink]
    (loop [tuple (produce this)]
      (when tuple
        (.add ^Collection sink tuple)
        (recur (produce this)))))
  (reset [_]
    (set! i 0))
  (total [_] 0))

(defn list-tuple-pipe
  [tuples]
  (ListTuplePipe. tuples 0))

(defn remove-end-scan
  [tuples]
  (if (.isEmpty ^Collection tuples)
    tuples
    (let [size (.size ^List tuples)
          s-1  (dec size)
          l    (.get ^List tuples s-1)]
      (if (identical? :datalevin/end-scan l)
        (do (.remove ^List tuples s-1)
            (recur tuples))
        tuples))))

(deftype OrJoinTuplePipe [^List tuples
                          ^long bound-idx
                          ^HashMap or-by-bound
                          ^long free-var-idx
                          ^long tuple-len
                          ^:unsynchronized-mutable ^long i
                          ^:unsynchronized-mutable ^objects current
                          ^:unsynchronized-mutable ^List matches
                          ^:unsynchronized-mutable ^long j]
  ITuplePipe
  (pipe? [_] true)
  (finish [_] nil)
  (produce [_]
    (loop []
      (if (and matches (< j (.size ^List matches)))
        (let [^objects or-tuple (.get ^List matches j)
              fv                (aget or-tuple free-var-idx)
              ^objects joined   (object-array (inc tuple-len))]
          (System/arraycopy current 0 joined 0 tuple-len)
          (aset joined tuple-len fv)
          (set! j (inc j))
          joined)
        (when (< i (.size ^List tuples))
          (let [^objects in-tuple (.get ^List tuples i)
                bv                (aget in-tuple bound-idx)
                ^List m           (.get ^HashMap or-by-bound bv)]
            (set! i (inc i))
            (if (and m (pos? (.size m)))
              (do (set! current in-tuple)
                  (set! matches m)
                  (set! j 0)
                  (recur))
              (do (set! current nil)
                  (set! matches nil)
                  (set! j 0)
                  (recur))))))))
  (add-batch [_ _]
    (u/raise "Cannot add tuples to an or-join input pipe" {}))
  (drain-to [this sink]
    (loop [t (produce this)]
      (when t
        (.add ^Collection sink t)
        (recur (produce this)))))
  (reset [_]
    (set! i 0)
    (set! current nil)
    (set! matches nil)
    (set! j 0))
  (total [_] 0))

(defn or-join-tuple-pipe
  [tuples bound-idx or-by-bound free-var-idx tuple-len]
  (OrJoinTuplePipe. tuples bound-idx or-by-bound free-var-idx
                    tuple-len 0 nil nil 0))

)) ;; end reader-conditional split
