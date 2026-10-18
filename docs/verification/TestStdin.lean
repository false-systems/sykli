/- Finite one-shot test input, prepared before spawn. Lean 4.35.0.
   Models ownership/order, not filesystem or Rust implementation correctness. -/
namespace TestStdin

structure Prepared where
  bytes : List UInt8
  offset : Nat
  deriving DecidableEq

def prepare (bytes : List UInt8) : Prepared := ⟨bytes, 0⟩
def readable (input : Prepared) : List UInt8 := input.bytes.drop input.offset

-- CHECK 1: the child sees exactly the prepared bytes, from the beginning.
theorem exact_input (bytes : List UInt8) : readable (prepare bytes) = bytes := by
  simp [readable, prepare]

inductive Outcome where
  | exit (code : Nat)
  | spawnError
  deriving DecidableEq

structure Collected where
  outcome : Outcome
  parentWritesAfterSpawn : Nat
  commandOwnsInput : Bool
  temporaryExists : Bool
  deriving DecidableEq

-- The command owns only a prepared read handle. No parent-side pipe writer
-- exists; collection does not reinterpret an exit code as a test pass.
def collect (_input : Prepared) (outcome : Outcome) : Collected :=
  ⟨outcome, 0, true, true⟩

-- CHECK 2: any early exit is preserved without a competing parent write.
theorem early_exit_is_observed (bytes : List UInt8) (code : Nat) :
    (collect (prepare bytes) (.exit code)).outcome = .exit code ∧
    (collect (prepare bytes) (.exit code)).parentWritesAfterSpawn = 0 := by
  exact ⟨rfl, rfl⟩

def closeCommand (r : Collected) : Collected := { r with commandOwnsInput := false }
def removeTemporary (r : Collected) : Collected :=
  if r.commandOwnsInput then r else { r with temporaryExists := false }

-- CHECK 3: on every collected result (including spawn failure), close the
-- command's retained handle before removal. Preserve the original outcome.
theorem cleanup_after_collection (bytes : List UInt8) (outcome : Outcome) :
    let final := removeTemporary (closeCommand (collect (prepare bytes) outcome))
    final.commandOwnsInput = false ∧ final.temporaryExists = false ∧
    final.outcome = outcome := by
  exact ⟨rfl, rfl, rfl⟩

#print axioms exact_input
#print axioms early_exit_is_observed
#print axioms cleanup_after_collection
end TestStdin
