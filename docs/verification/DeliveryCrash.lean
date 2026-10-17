/-
Small model of one dispatched delivery with no recorded end, no explicit
--reapply or --assume-ended, and a correctly observed apply lease. Not a model
of the OS, disk durability, all delivery dimensions, or the Rust implementation.
Run: lean docs/verification/DeliveryCrash.lean (Lean 4.35.0, no dependencies).
-/
namespace DeliveryCrash

inductive Boundary where
  | beforeStarted | afterStarted
  deriving DecidableEq, Repr

inductive Phase where
  | dispatched | applying | lost
  deriving DecidableEq, Repr

inductive Decision where
  | refuseLive | reconcile
  deriving DecidableEq, Repr

structure State where
  started : Bool
  childReady : Bool
  alive : Bool
  coordinator : Bool
  applies : Nat
  deriving DecidableEq, Repr

def spawned : State := ⟨false, false, true, true, 1⟩
def childReady (s : State) : State := { s with childReady := true }
def recordStarted (s : State) : State := { s with started := true }
def crash (s : State) : State := { s with coordinator := false }

def phase (s : State) : Phase :=
  if s.coordinator || s.alive then
    if s.started then .applying else .dispatched
  else .lost

def resume (s : State) : Decision :=
  if s.coordinator || s.alive then .refuseLive else .reconcile

-- The two scheduler orders have the same child-ready signal but different
-- durable evidence at the instant the old helper kills the coordinator.
def early : State := crash (childReady spawned)
def late : State := crash (childReady (recordStarted spawned))
theorem old_wait_is_not_a_journal_barrier :
    early.childReady = true ∧ late.childReady = true ∧
    phase early = .dispatched ∧ phase late = .applying := by decide

-- New barrier: child ready AND the selected journal boundary. The coordinator
-- is paused there, so the boundary cannot move while the child reaches its latch.
def atBoundary (b : Boundary) (s : State) : Prop :=
  s.coordinator = true ∧ s.childReady = true ∧ s.alive = true ∧
  s.started = (b == .afterStarted)

-- SPEC CHECK 1: a live orphan without a Started record is not called applying,
-- and cannot be resumed. Count is unchanged, for every count and ready state.
theorem before_started_crash (s : State) (h : atBoundary .beforeStarted s) :
    phase (crash s) = .dispatched ∧ resume (crash s) = .refuseLive ∧
    (crash s).applies = s.applies := by
  rcases s with ⟨started, ready, alive, coordinator, count⟩
  simp_all [atBoundary, phase, resume, crash]

-- SPEC CHECK 2: a live orphan with Started recorded is applying, not complete;
-- it is equally forbidden to resume. Neither crash manufactures an apply.
theorem after_started_crash (s : State) (h : atBoundary .afterStarted s) :
    phase (crash s) = .applying ∧ resume (crash s) = .refuseLive ∧
    (crash s).applies = s.applies := by
  rcases s with ⟨started, ready, alive, coordinator, count⟩
  simp_all [atBoundary, phase, resume, crash]

inductive Observation where
  | attributed | inconclusive
  deriving DecidableEq, Repr

inductive Effect where
  | unknown | observed
  deriving DecidableEq, Repr

structure Recovery where
  state : State
  effect : Effect
  deriving DecidableEq, Repr

-- The caller selects this transition only after resume permits reconciliation.
-- An observation may establish effect, never manufacture another apply.
def reconcile (r : Recovery) (o : Observation) : Recovery :=
  { r with effect := if o == .attributed then .observed else .unknown }

def recoverMany (r : Recovery) (observations : List Observation) : Recovery :=
  observations.foldl reconcile r

theorem reconciliation_preserves_count (r : Recovery) (os : List Observation) :
    (recoverMany r os).state.applies = r.state.applies := by
  induction os generalizing r with
  | nil => rfl
  | cons o os ih => exact ih (reconcile r o)

-- SPEC CHECK 3: after the orphan ends, both journal windows are lost (not
-- passed), resume reconciles, and arbitrarily many observations never reapply.
theorem recovery_without_duplicate (started ready : Bool) (count : Nat)
    (os : List Observation) :
    let s : State := ⟨started, ready, false, false, count⟩
    phase s = .lost ∧ resume s = .reconcile ∧
    (recoverMany ⟨s, .unknown⟩ os).state.applies = count ∧
    (reconcile ⟨s, .unknown⟩ .inconclusive).effect = .unknown ∧
    (reconcile ⟨s, .unknown⟩ .attributed).effect = .observed := by
  dsimp
  simp [phase, resume, reconciliation_preserves_count, reconcile]

#print axioms before_started_crash
#print axioms after_started_crash
#print axioms recovery_without_duplicate
end DeliveryCrash
