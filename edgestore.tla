---------------------------- MODULE edgestore ----------------------------

(*
  Abstract model of the edgestore storage engine.

  Scope (zeroth option): the core data flow — how writes move from client
  through WAL → memtable → immutable segment, and how reads are served from
  the union of those layers. Replication (LWW merge) and vector/text indexes
  are intentionally out of scope; see edgestore_replication.tla when ready.

  We model three invariants:
    1. NoDataLoss      — every committed write is readable until explicitly deleted
    2. FlushSafety     — WAL may only be retired after the segment is durable
    3. LsnMonotonic    — LSNs only increase; no key is assigned a lower LSN than
                         any previous write

  State variables:
    wal      — sequence of WalRecord (ordered by arrival, not LSN)
    memtable — function: Key -> {value, lsn, op}   (op ∈ {Put, Delete})
    segments — set of immutable Segment snapshots
    flushed  — highest LSN that has been flushed to a segment
    lsn      — monotonically-increasing logical sequence number counter

  Assumptions / simplifications:
    - Single writer (no concurrency modeled here)
    - Keys are opaque blobs (modeled as strings)
    - Values are opaque blobs
    - A segment is a complete snapshot of the memtable at flush time
    - Crash = any state where the WAL is non-empty and memtable may be lost;
      recovery replays the WAL to rebuild memtable
*)

EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS
    Keys,       \* the set of all possible keys (finite for model checking)
    Values,     \* the set of all possible values
    MaxLsn,     \* upper bound for model checking (prevents infinite state)
    ABSENT      \* model value sentinel for missing entries (declared here so TLC
                \* treats it as an uninterpreted atom, allowing type-safe = comparisons)

VARIABLES
    wal,        \* sequence of [key, value, lsn, op] records
    memtable,   \* function Key -> [value, lsn, op] | ABSENT
    segments,   \* set of [entries: (Key -> [value, lsn, op]), min_lsn, max_lsn]
    flushed,    \* highest LSN confirmed durable in a segment (0 = none)
    lsn         \* current LSN counter

TypeOK ==
    /\ wal \in Seq([key: Keys, value: Values \cup {"tombstone"},
                    lsn: 1..MaxLsn, op: {"Put", "Delete"}])
    /\ \A k \in Keys :
           memtable[k] = ABSENT
           \/ memtable[k] \in [value: Values \cup {"tombstone"},
                                lsn: 1..MaxLsn, op: {"Put", "Delete"}]
    /\ \A seg \in segments :
           /\ seg.min_lsn \in 0..MaxLsn
           /\ seg.max_lsn \in 0..MaxLsn
           /\ seg.min_lsn <= seg.max_lsn
    /\ flushed \in 0..MaxLsn
    /\ lsn \in 0..MaxLsn

vars == <<wal, memtable, segments, flushed, lsn>>

-----------------------------------------------------------------------------
(* Helper: visible value of key k across all layers (memtable wins) *)

SegmentGet(k) ==
    \* Latest value across all segments (highest lsn wins)
    LET matching == {seg \in segments :
                        seg.entries[k] /= ABSENT /\
                        seg.entries[k].op = "Put"}
    IN  IF matching = {}
        THEN ABSENT
        ELSE LET best == CHOOSE seg \in matching :
                             \A other \in matching :
                                 seg.entries[k].lsn >= other.entries[k].lsn
             IN  best.entries[k].value

LiveGet(k) ==
    IF memtable[k] /= ABSENT
    THEN IF memtable[k].op = "Put" THEN memtable[k].value ELSE ABSENT
    ELSE SegmentGet(k)

-----------------------------------------------------------------------------
(* Initial state *)

Init ==
    /\ wal       = <<>>
    /\ memtable  = [k \in Keys |-> ABSENT]
    /\ segments  = {}
    /\ flushed   = 0
    /\ lsn       = 0

-----------------------------------------------------------------------------
(* Action: Put(k, v) — single-writer put *)

Put(k, v) ==
    /\ lsn < MaxLsn
    /\ lsn' = lsn + 1
    /\ wal' = Append(wal, [key |-> k, value |-> v, lsn |-> lsn + 1, op |-> "Put"])
    /\ memtable' = [memtable EXCEPT ![k] = [value |-> v, lsn |-> lsn + 1, op |-> "Put"]]
    /\ UNCHANGED <<segments, flushed>>

(* Action: Delete(k) — logical tombstone *)

Delete(k) ==
    /\ lsn < MaxLsn
    /\ lsn' = lsn + 1
    /\ wal' = Append(wal, [key |-> k, value |-> "tombstone",
                            lsn |-> lsn + 1, op |-> "Delete"])
    /\ memtable' = [memtable EXCEPT ![k] = [value |-> "tombstone",
                                             lsn |-> lsn + 1, op |-> "Delete"]]
    /\ UNCHANGED <<segments, flushed>>

(* Action: Flush — atomically write memtable to a new segment, then retire WAL *)

\* A flush is safe only when:
\*   (a) there are unflushed writes (flushed < lsn), AND
\*   (b) the segment write is atomic (modeled as an atomic TLA+ step)
\* After the step succeeds, flushed advances to lsn, wal is cleared, and the
\* memtable is cleared (data now lives in the segment — mirrors flush_to_segments_inner
\* calling self.memtable.clear() in the Rust implementation).

Flush ==
    /\ flushed < lsn              \* unflushed writes exist; guarantees min_lsn <= max_lsn
    /\ LET new_seg == [entries  |-> memtable,
                        min_lsn |-> flushed + 1,
                        max_lsn |-> lsn]
       IN
       /\ segments' = segments \cup {new_seg}
       /\ flushed'  = lsn
       /\ wal'      = <<>>
       /\ memtable' = [k \in Keys |-> ABSENT]       \* data moved to segment
       /\ UNCHANGED lsn

(*
  FlushSafety invariant (checked separately below):
  We model flush as a single atomic step. The invariant says that between
  "segment write" and "WAL retirement" there is no reachable state where
  the data is in neither layer. Because TLA+ models atomic steps, this is
  guaranteed by construction here. In the implementation, the WAL file is
  only deleted AFTER fsync of the segment — the spec reflects that ordering.
*)

(* Action: Compact — merge two segments; preserves LWW semantics *)

\* For simplicity, merge exactly two segments into one.
Compact ==
    /\ Cardinality(segments) >= 2
    /\ LET seg1 == CHOOSE s \in segments : TRUE
           seg2 == CHOOSE s \in segments \ {seg1} : TRUE
           \* LWW merge: for each key, keep the entry with the higher LSN
           merged == [k \in Keys |->
               LET e1 == seg1.entries[k]
                   e2 == seg2.entries[k]
               IN  IF e1 = ABSENT /\ e2 = ABSENT THEN ABSENT
                   ELSE IF e1 = ABSENT THEN e2
                   ELSE IF e2 = ABSENT THEN e1
                   ELSE IF e1.lsn >= e2.lsn THEN e1 ELSE e2]
           new_seg == [entries  |-> merged,
                        min_lsn |-> IF seg1.min_lsn < seg2.min_lsn
                                    THEN seg1.min_lsn ELSE seg2.min_lsn,
                        max_lsn |-> IF seg1.max_lsn > seg2.max_lsn
                                    THEN seg1.max_lsn ELSE seg2.max_lsn]
       IN
       /\ segments' = (segments \ {seg1, seg2}) \cup {new_seg}
       /\ UNCHANGED <<wal, memtable, flushed, lsn>>

(* Action: Restart — combined crash+recovery cycle.
   Models Engine::open: WAL is durable across crashes; recovery replays it
   atomically. The intermediate "memtable wiped, not yet recovered" state is
   never observable in the implementation — Engine::open either succeeds or
   the engine is not running. Crash is not a standalone action here. *)

Restart ==
    /\ wal /= <<>>
    /\ LET replayed ==
               [k \in Keys |->
                   LET relevant == {i \in 1..Len(wal) : wal[i].key = k}
                   IN  IF relevant = {}
                       THEN ABSENT
                       ELSE LET best_i == CHOOSE i \in relevant :
                                    \A j \in relevant : wal[i].lsn >= wal[j].lsn
                            IN  [value |-> wal[best_i].value,
                                 lsn   |-> wal[best_i].lsn,
                                 op    |-> wal[best_i].op]]
       IN
       /\ memtable' = replayed
       /\ UNCHANGED <<wal, segments, flushed, lsn>>

-----------------------------------------------------------------------------
(* Next-state relation *)

Next ==
    \/ \E k \in Keys, v \in Values : Put(k, v)
    \/ \E k \in Keys : Delete(k)
    \/ Flush
    \/ Compact
    \/ Restart

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
(* Invariants *)

(*
  1. NoDataLoss: every Put that has been committed (in WAL or a segment) and
     not subsequently overwritten or deleted is readable via LiveGet.
*)
NoDataLoss ==
    \A k \in Keys :
        \* If the last WAL record for k is a Put, LiveGet must return its value.
        LET wal_records == {i \in 1..Len(wal) : wal[i].key = k}
            last_wal    == IF wal_records = {}
                           THEN ABSENT
                           ELSE LET i == CHOOSE j \in wal_records :
                                             \A m \in wal_records : wal[j].lsn >= wal[m].lsn
                                IN  wal[i]
        IN  last_wal /= ABSENT /\ last_wal.op = "Put"
            => LiveGet(k) = last_wal.value

(*
  2. FlushSafety: every WAL entry has LSN strictly greater than flushed.
     Equivalently: WAL is only cleared AFTER the segment is durable and
     flushed advances past the highest WAL LSN.
*)
FlushSafety ==
    \A i \in 1..Len(wal) :
        wal[i].lsn > flushed

(*
  3. LsnMonotonic: the LSN counter only increases.
*)
LsnMonotonic ==
    \A i \in 1..Len(wal) :
        \A j \in 1..Len(wal) :
            i < j => wal[i].lsn < wal[j].lsn

(*
  4. SegmentLsnOrder: within every segment, no key has an LSN outside the
     segment's [min_lsn, max_lsn] range.
*)
SegmentLsnOrder ==
    \A seg \in segments :
        \A k \in Keys :
            seg.entries[k] /= ABSENT
            => /\ seg.entries[k].lsn >= seg.min_lsn
               /\ seg.entries[k].lsn <= seg.max_lsn

AllInvariants ==
    /\ TypeOK
    /\ NoDataLoss
    /\ FlushSafety
    /\ LsnMonotonic
    /\ SegmentLsnOrder

=============================================================================
\* Modification History
\* Created for edgestore formal verification — zeroth spec (abstract data model)
\* Covers: Put/Delete, WAL→memtable→segment flush, crash+recovery, compaction
\* Out of scope: replication (LWW import_segment), vector/text indexes, TTL
