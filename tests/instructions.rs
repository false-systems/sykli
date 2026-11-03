//! What every agent session here loads before its first task: this
//! repository's rules, and no more than it needs.
//!
//! Claude Code reads AGENTS.md by itself only where no CLAUDE.md sits at or
//! above the folder it starts in, and the folder holding the repositories
//! has one. So CLAUDE.md must import AGENTS.md, or Claude never sees these
//! rules. Codex reads AGENTS.md directly.

/// CLAUDE.md and AGENTS.md together, in bytes: about 1,500 tokens. Detail
/// an agent needs only for some tasks belongs in the README or `docs/`.
const BUDGET: usize = 6_000;

fn read(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn claude_code_loads_agents_md_and_both_stay_small() {
    let claude = read("CLAUDE.md");
    assert!(
        claude.lines().any(|line| line.trim() == "@AGENTS.md"),
        "CLAUDE.md must import AGENTS.md on a line of its own (`@AGENTS.md`), \
         or Claude Code never loads this repository's rules"
    );
    let size = claude.len() + read("AGENTS.md").len();
    assert!(
        size <= BUDGET,
        "CLAUDE.md and AGENTS.md hold {size} bytes, over the {BUDGET} every session \
         loads before its first task: move detail to the README or docs/"
    );
}
