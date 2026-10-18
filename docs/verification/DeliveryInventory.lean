/- Complete-or-error inventory for delivery safety decisions. Lean 4.35.0.
   This abstracts filesystem reads and journal validation, not their implementation. -/
namespace DeliveryInventory

inductive Entry where
  | valid (id : Nat)
  | unreadable (id : Nat)
  deriving DecidableEq, Repr

def collect : List Entry → Except Nat (List Nat)
  | [] => .ok []
  | .unreadable id :: _ => .error id
  | .valid id :: rest => (collect rest).map (id :: ·)

-- CHECK 1: an unreadable delivery anywhere prevents a successful inventory;
-- an earlier unreadable entry may be the error reported first.
theorem unreadable_blocks (before after : List Entry) (id : Nat) :
    ∀ ids, collect (before ++ .unreadable id :: after) ≠ .ok ids := by
  induction before with
  | nil => simp [collect]
  | cons entry before ih =>
    cases entry with
    | unreadable other => simp [collect]
    | valid other =>
      simp only [List.cons_append, collect]
      cases h : collect (before ++ .unreadable id :: after) with
      | error e => simp [Except.map]
      | ok ids => exact False.elim (ih ids h)

-- CHECK 2: complete readable input preserves every entry, including ordering
-- and multiplicity; unlike filter_map, validation never silently drops one.
theorem complete_preserves (ids : List Nat) :
    collect (ids.map Entry.valid) = .ok ids := by
  induction ids with
  | nil => rfl
  | cons id ids ih => simp [collect, ih, Except.map]

structure Store where
  applies : Nat
  sequence : Nat
  retained : List Nat
  deriving DecidableEq, Repr

inductive Action where
  | apply | rebuild | prune
  deriving DecidableEq, Repr

-- Success only permits the caller's further safety checks. These representative
-- mutations deliberately differ; every action must propagate an inventory error.
def guarded (inventory : Except Nat (List Nat)) (action : Action) (s : Store) : Store :=
  match inventory with
  | .error _ => s
  | .ok ids => match action with
    | .apply => { s with applies := s.applies + 1 }
    | .rebuild => { s with sequence := ids.foldl max 0 + 1 }
    | .prune => { s with retained := ids }

-- CHECK 3: incomplete discovery authorizes none of the three mutations,
-- for any action and any store; real pruning also requires existing guards.
theorem failure_preserves (id : Nat) (action : Action) (s : Store) :
    guarded (.error id) action s = s := by rfl

-- Absence is not unreadability. Only a genuinely absent store yields [];
-- I/O failure, malformed journals and incomplete publication yield an error.
inductive Root where
  | absent | failed (id : Nat) | present (entries : List Entry)

def scan : Root → Except Nat (List Nat)
  | .absent => .ok []
  | .failed id => .error id
  | .present entries => collect entries

example : scan .absent = .ok [] := rfl
example (id : Nat) : scan (.failed id) ≠ scan .absent := by simp [scan]

#print axioms unreadable_blocks
#print axioms complete_preserves
#print axioms failure_preserves
end DeliveryInventory
