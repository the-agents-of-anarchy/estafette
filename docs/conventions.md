# Conventions

## commit & merge

### git-from-root
when    running any git or shell command
do      run git as `git -C <absolute root>`; stage by explicit path and name every path on the commit; write every path absolute; keep every command inside the session's worktree when it runs in one
never   bare or relative git; `git add -A`; a commit taking whatever the index holds; `cd` inside a compound command; step out of that worktree
why     The Bash tool resets cwd after each call and the harness re-pins the shell into worktrees; a failed hook leaves paths staged; the owner's zoxide alias aborts a compound `cd`.
ground  owner ruling 2026-10-23

### linear-integration
when    integrating any branch into another, reviewing a worktree branch, or resuming or continuing its agent
do      rebase onto the target, then fast-forward; run `git -C <worktree> log main..HEAD` and rebase a branch cut from an old SHA; run `git -C <worktree> branch --show-current`
never   a merge commit; review a stale branch; trust commits a continued agent made on a detached HEAD
why     Linear history on every branch; worktrees branch from an old SHA and resumed agents commit on a detached HEAD.
ground  owner ruling 2026-10-23

### commits
when    committing or configuring pre-commit
do      bump the version in the same commit as the change it describes; commit a delivery milestone unprompted; commit a spec or reference change before any agent transcribes from it; let every pre-commit hook run; configure pre-commit with `trailing-whitespace`, `end-of-file-fixer`, `check-yaml`, `check-added-large-files` and `records` (`just records`) beside `just check`
never   a version bump in a commit of its own; wait to be asked to commit; dispatch against uncommitted edits; `--no-verify` or any other skipped hook
why     A version points at its content; uncommitted edits are invisible in every worktree; owner-mandated hooks bind.
ground  owner ruling 2026-10-23

## code

### compiler-strict
when    configuring lints or writing Rust
do      set `unsafe_code = "forbid"` in `[workspace.lints.rust]`; name every variant in a match on a closed enum, so a new variant breaks the build; treat rust-analyzer diagnostics as errors and read them through the LSP before moving on
never   a silent wildcard arm; ship anything a layer (rust-analyzer, clippy, tests, CI) rejects
why     Let the compiler and rust-analyzer catch what no lint in the table enforces: unsafe code and a missed enum variant.
ground  owner ruling 2026-10-23

### interfaces-and-refusals
when    designing a function, type, refusal, error or configuration field
do      deep module with required parameters; one predicate per question; discriminate in the type; refuse before any side effect, naming the narrowed input set in the message; have code read every configuration field, or delete the field; refuse unknown keys (`deny_unknown_fields`); refuse a misconfigured network or storage dependency at config load (shape, secrets) or at construction (a reachability probe on its own deadline); print an anticipated failure, a typed error meant for whoever ran the command, as its message alone
never   an unverifiable universal claim in user-facing text; a field nothing reads; discover a misconfiguration at first use; admit a std or I/O error to the anticipated set to silence a backtrace
why     Type-driven design at every boundary; a field nothing reads misleads; a misconfiguration found late hangs before it fails; widening the anticipated set silences real bugs.
ground  owner ruling 2026-10-23
except  unknown keys of a card, which the binary ignores (spec:580); a host or relay spec 3.15 lets be dead (tests 23, 25), which is not refused

### file-paths-and-writes
when    code needs a file path, writes a large artifact, or skips work because one exists
do      derive every file path from the base directories of spec 3.3 through one module; write a temp file and rename it over the final name; guard on the final name alone
never   a hardcoded path; a flag naming a path the configuration already determines; trust existence or `stat`
why     Two parts of one run must agree where a file is; a truncated file passes an existence guard.
ground  owner ruling 2026-10-23
except  `--cards-dir` and `ESTAFETTE_CARDS_DIR`, which spec:534 requires

### console-output
when    writing to the console, writing help, or checking a configuration value or rendered string
do      result on stdout, run record (logs, progress, failures) on stderr, both through the CLI layer's streams; state in help what the command does, reads, produces and refuses; judge help and values by rendering or loading them
never   a failure on stdout; `print!` or `println!` beneath the CLI layer; grep source for what the reader meets
why     On the MCP stdio pipe stdout is the protocol; source holds what never renders and omits what clap synthesizes.
ground  owner ruling 2026-10-23

### edits-verified-by-parsing
when    authoring structured YAML or writing a multi-file edit script
do      write structured YAML in block form, one key per line; verify a reformat by parsing before and after; in a multi-file edit script collect every replacement, assert each phrase present and unique, then write once
never   inline mappings; eyeballing a reformat's diff; write a file before every assertion passes
why     Inline YAML and half-applied edit scripts were wrong and invisible until parsed or asserted.
ground  owner ruling 2026-10-23

### build-configuration
when    configuring the workspace or the release build
do      once the workspace holds more than one package, share one version through `[workspace.package]` and `version.workspace = true`; release build: `[profile.release]` with `strip = true`, `lto = "thin"`, `panic = "abort"`; for thin LTO on the owner's Mac add `rustflags = ["-C", "link-arg=-Wl,-ld_classic"]` under `[target.aarch64-apple-darwin]` in `.cargo/config.toml`
why     Spec:351 versions the packages together; `panic = "abort"` fits the no-panic stance; Apple ld asserts on thin LTO.
ground  owner ruling 2026-10-23

### dependency-safety
when    gating, patching or installing dependencies and tools
do      run `cargo audit` in the gate and in CI; ignore an advisory only with a documented reason in `.cargo/audit.toml`; pin the version of every tool the gate or a recipe calls that is not a project dependency; run a patched crate's own test suite before adding or keeping its `[patch.crates-io]` entry; install cargo tools with `cargo binstall`
never   an ignored advisory without its reason; an unpinned tool in the gate; a patch whose own suite was not run; `cargo install`
why     Public repository; spec D48 already chose crates by RUSTSEC advisory; an unpinned tool changes the gate under the same commit.
ground  owner ruling 2026-10-23

### credentials-unread
when    handling a credential, an environment variable or `.env`
do      source a credential file to load it; hand credentials on stdin; test a variable with a construct printing only yes or no; treat `.env` as secret from every agent, the lead included
never   read a credential file's contents; pass a credential as an argument; print the environment, filtered or not
why     A filter by name still prints a secret embedded in a URL.
ground  owner ruling 2026-10-23

### destructive-operations
when    killing or waiting on a process, running a destructive command, or backing up a file before mutation
do      resolve a process's exact PID and verify it is yours before killing it; match a wait loop on the executable; inspect what a failed destructive command left behind before continuing; check a backup for NUL bytes and expected structure before use
never   `pkill -f` or any pattern kill; a `pgrep -f` wait that matches its own shell; an unverified `cp` backup; `git checkout <file>` to revert a probe, which destroys other uncommitted edits
why     A pattern kill takes others' runs; a self-matching wait never clears; two `cp` backups held only NUL bytes.
ground  owner ruling 2026-10-23

### shell-on-the-mac
when    writing a shell command or script on the owner's Mac
do      redirect a long command, or one whose exit code gates a decision, to a file (`cmd > log 2>&1`) and read the file; bound a wait by polling a done-marker in a bounded loop; edit in place with `sed -i '' '<script>' <file>`; quote each path; pass flags literally or through an array; `setopt nullglob` before a glob that may match nothing; print with `printf '%s\n'`; feed bash a script as `bash -c "$(cat <<'EOF' ... EOF)"`; search with `/usr/bin/grep`
never   pipe such a command through `| tail` or `| tee`; poll it; `timeout` or `gtimeout`, which do not exist there; `sed -i '<script>' <file>`; an unquoted `$var` relied on to split; `echo ===`; `echo "$json"`; `bash <<'EOF'` around a command that reads stdin; the aliased `grep`
why     A pipe returns its last command's exit code; macOS lacks `timeout` and GNU sed; zsh never word-splits `$var`; a stdin-reading command swallows a piped script.
ground  owner ruling 2026-10-23

### scripts-and-paths
when    saving a script or naming a directory or file
do      copy a build or probe script that will run again into the tracked tree, with the scratch root as an environment variable; name directories and files with hyphens in place of colons (session ids included)
never   leave such a script only under /tmp, which a reboot clears; a colon in a path
why     A reboot clears /tmp; a colon breaks cargo on macOS through the `DYLD_FALLBACK_LIBRARY_PATH` separator.
ground  owner ruling 2026-10-23

## measuring & quoting

### quote-figures
when    reporting a count or quoting a measured figure
do      derive every count with a tool (`wc`, `git rev-list --count`) and state the selection rule; name with every measured figure its machine, environment (versions read inside it), data scale and code state
never   a count from prose or memory; carry a figure to another machine or scale unmarked
why     One machine's figures do not describe another's; counts from prose are wrong.
ground  owner ruling 2026-10-23

### verify-before-building
when    a claim, result, grep hit, specification or explanation becomes load-bearing
do      verify a load-bearing claim in source or by reproduction; before writing or advising on what a crate provides, read the installed crate's source or metadata (`~/.cargo/registry`, `cargo doc`); check a welcome result harder than a disappointing one before quoting it; read a grep hit's record header and section heading before acting on it; check a specification against its sources before freezing or building on it; restart a retracted explanation's investigation from what was reproduced
never   relay a claim as fact; advise from memory; reimplement a helper an installed crate provides; publish on agreement; quote or act on a bare grep hit; count on the review chain to catch a wrong spec; reason forward from a retracted story
why     A false hazard crossed four readers; reviews passed work against a wrong spec; a withdrawn mechanism survives when reasoning continues from it.
ground  owner ruling 2026-10-23

## reviewing & tests

### gate-discipline
when    running the gate, calling a suite green, integrating, or claiming a unit done
do      run a suite twice in succession before calling it green; after every integration run `just check` on main, the same commands, recording the SHA and reading the ignored-test list; record a failing gate or lint with its output and fix the code; run the gate first for a baseline when starting a unit; claim done only with runnable code, passing tests and committed files, showing the full test output; treat a failing test as a bug until shown otherwise; run clippy once without and once with the `relay` feature (spec D47)
never   a subset; one passing run; a worktree result as proof; weaken an assertion, narrow a grep, delete a test, silence a lint, edit lint configuration or widen an ignore list to pass a gate; claim done without that evidence; blame the machine; a single pass over one feature set
why     Leftover state passes once; a gate passed by suppression measures nothing; the relay compiles only under its feature; with the owner away, completion evidence is the contract.
ground  owner ruling 2026-10-23

### tests-that-discriminate
when    writing a test or pinning a fix
do      revert each fix and require the named test to fail; pick values where every plausible wrong field disagrees with the right one; probe values, not only absences; seed by loop index; reach the real condition through the real command; gate a unit that depends on a real network or process on a real run, reported verbatim; name in each test the spec section or spec 3.15 test number it proves
never   accept a pin you only reasoned about; convenient values; a seed from a hash or iteration order randomized per process (`RandomState`, `HashMap`); construct the error and assert the handler; merge such a unit on mocked evidence
why     Weak fixtures let mutations survive; iroh and the Claude Code bridge are real boundaries; the spec numbers its 28 acceptance tests.
ground  owner ruling 2026-10-23

### todo-stubs-uncommitted
when    designing signatures
do      stub bodies with `todo!()`; remove every one before the commit
never   count a `todo!()` in an uncommitted tree as a fault
why     The tdd skill's stub step meets `todo = "deny"`, which the gate applies on commit.
ground  owner ruling 2026-10-23

### developer-tooling
when    changing or reviewing developer tooling (`just` recipes, scripts)
do      verify a developer script (a `just` recipe, a skill's script) by running it; review developer tooling with implementer, then spec review
never   tests for developer scripts; a quality link on developer tooling
why     People read a script's output; nothing in the product imports it.
ground  owner ruling 2026-10-23

### verdicts
when    briefing a reviewer, writing, correcting or accepting a verdict
do      hand a reviewer SHAs and ranges; the reviewer reads `git show` and `git diff` from the root checkout; read a verdict or report file's body, size and headings before trusting its first line; append a verdict's correction beside it; the original stays of record; re-read each corrected clause against the code; withdraw a finding that invents a convention or moves a refusal
never   a path another agent is writing; edit a verdict in place; check a corrected clause against the finding it answers; manufacture a finding to look thorough
why     A finding against a moving file is unfalsifiable; a 263-byte report read Ready to merge over two Important findings; a verdict edited in place was verified against itself.
ground  owner ruling 2026-10-23

### receiving-review
when    receiving review findings or a correction
do      read every item, verify each, hold while any is unclear and ask once, push back with evidence, implement one at a time with a test run after each; act before speaking
never   gratitude, agreement or any other performative reply
ground  owner ruling 2026-10-23

### ci-watch
when    a push to main awaits CI
do      match the run to its commit with `gh run list --branch main --commit <sha>` on a recurring check; on failure delegate log diagnosis to a sonnet agent
never   read the log in the lead's context
ground  owner ruling 2026-10-23

## writing craft

### prose
when    writing anything another reads, or ending a run of rulings or a behaviour change
do      write for very tired, brilliant engineers with no domain expertise; compress beyond what they need spelled out; spend tokens on decisions; on a quiet tick a short status, on a state change a full render; give anything the owner reads canonical names and plain descriptions; after a run of rulings or a behaviour change, grep the old claim across every document and fix each stale one
never   jargon; tutorial tone; re-derivable state; invented codenames or unit labels; fix only the paragraph first noticed
why     Both owner forms compose; surgical edits left contradictions in two documents.
ground  owner ruling 2026-10-23
except  silence under work-silently, which writes nothing

### sources-and-authorship
when    citing a source or writing a rule that claims authority
do      check a source's authorship before citing it; mark each rule's author; retire on sight a rule that is the operator's design
never   cite an explicitly Russian-authored source, whoever suggested it; let an operator's rule stand under the owner's name
why     Owner rule on every project; honest provenance on a public repository.
ground  owner ruling 2026-10-23

### claude-code-material
when    reading Claude Code docs, writing skill, agent or hook text, or rendering a Mermaid diagram
do      fetch a `code.claude.com/docs` page as markdown by appending `.md`; when `claude --version` is ahead of the local changelog, read the raw GitHub `CHANGELOG.md`; structure skill, agent and hook text with XML tags named imperatively; pair every directive with a prohibition; write hook output as prose; use Claude Code's native vocabulary; render Mermaid offline with `merman-cli -f png`
never   a directive without its prohibition; a Mermaid classDef named `label`
why     The bridge targets Claude Code and the local changelog lags; merman's SVG carries HTML labels and `label` collides with Mermaid's own class.
ground  owner ruling 2026-10-23

### records-state
when    keeping project state, writing scratch, or closing a span
do      keep all project state in the repository; keep the scratch home the schema names in `.gitignore`; at span close write the memory records first, then classify every recon artifact as discard, promote or reference
never   harness memory or any store outside the repository; commit a scratch file; close with an artifact unclassified
why     Sessions open with no memory; scratch is disposable and binds nothing.
ground  owner ruling 2026-10-23
except  credentials, which live outside the repository (docs/ground-truth.md:31)

## delegating agents

### agent-call
when    calling `Agent`
do      name `model`; opus for a high-risk or uncertain unit whatever its size; pass `isolation: "worktree"` on the Agent call
never   `inherit`; rely on the agent's frontmatter alone
why     `inherit` bills the lead's tier; four implementers landed on main despite frontmatter.
ground  owner ruling 2026-10-23

### briefs
when    writing any brief, or onboarding
do      open by commanding the agent to bind its stack and send its commitments, then work in the same turn; name `docs/conventions.md` binding in every brief touching source; name the skills it may load; put each constraint known at dispatch in the brief as a numbered, gated requirement; the interface spec verbatim in both briefs of a two-sided interface; "do not end the turn before the file write"; input lists as globs with the resolved list reported; the path to the data, with numbers in the brief marked suspect; list in a worktree-isolated worker's brief the absolute paths outside its worktree it may read (briefs, recon, uncommitted inputs); scope its writes to the worktree; state both halves; at onboarding read the primary sources once yourself; agents read them afterwards
never   a brief readable as awaiting approval; a recalled figure; a constraint sent by message; a read of an unlisted outside path
why     Subagents start in the session's start directory; parallel briefs invented two halves of one format; briefs and scratch exist in no worktree.
ground  owner ruling 2026-10-23

### reports
when    briefing an agent's final report or reading its binding report
do      require commitments, changes, every gate with pasted output, measurements, then what was not determined and what would settle it; accept "no class to close" as an outcome; require an agent reporting its binding to give each source's line count read, checked against `wc -l`
never   a report missing the not-determined part; trust a binding report alone
why     Subagents claimed full loads that stopped at the output cap.
ground  owner ruling 2026-10-23

### mid-run-corrections
when    correcting or updating a running agent, or answering its escalation
do      `TaskStop` a running agent before updating it, then send one self-contained message led by the delta; write a mid-run correction into the agent's brief file in the same turn, then message it; treat an agent's escalated question or contradiction stop as correct and answer it
never   a plain message racing the work; a correction living only in the agent's context; accept an agent's self-declared default or deadline
why     An agent reads its inbox at its next tool round; a second agent briefed from the file repeated a corrected defect.
ground  owner ruling 2026-10-23

### decomposition
when    decomposing, dispatching or continuing work for agents
do      count shared services (ports, containers, databases) in each write set; run disjoint sets concurrently; run a mechanical signature pass first; for one file, extract, rewrite to separate files and reassemble; estimate P50 and P90 wall time and tokens and plan on P90; keep fixed overhead under a third of a unit's cost; past the context window, start fresh with the delta alone; act directly where an agent's only output would be NEEDS_CONTEXT
never   judge units independent by files alone; parallel agents writing one file; plan on the median; dispatch an agent whose only output would be NEEDS_CONTEXT
why     A stage ends with its slowest unit; cache reads grow with turns squared; the Cargo workspace and test ports collide.
ground  owner ruling 2026-10-23

### agent-outcomes
when    a delegated run ends, goes quiet, or its claim needs verifying
do      verify every delegated run's units, reviews and merges against artifacts, a suborchestrator's included; check each notification's timestamp against current state; count agents against the dispatch list before declaring completion; judge an agent's liveness by commits, file mtimes and artifacts; probe by message; declare death only when the budget elapsed, the artifact is absent and the probe went unanswered; give a verifier the raw inputs with randomized IDs; verify a bold quantitative claim from a sonnet agent with an opus verifier
never   act on a notification as the work; read silence or an empty `ListAgents` as death; show a verifier the original report; seed a hypothesis
why     Notifications cross-wire and arrive days late; agents declared dead delivered minutes later; delegation hands off work, never responsibility.
ground  owner ruling 2026-10-23

### wakes-and-resumes
when    work is in flight, an agent is dispatched, or an agent stopped at a usage limit
do      while work is in flight keep a recurring heartbeat, at most hourly, its prompt one generic line; re-arm it on waking; journal only an event; arm a backstop per dispatch or continuation at the unit class's P90 with the ladder quiet, quiet, probe, stop; kill it in the turn its subject completes; resume an agent stopped at a usage limit by SendMessage, one agent at a time, cheapest finishing step first; when the limit stopped the session too, resume on the first turn back
never   facts or a journaling instruction in a wake prompt; a journal line saying nothing happened; leave a backstop after its subject completes; relaunch a usage-limited agent; resume all at once; a reset-aligned wake, which cannot be set
why     Session crons die and recurring ones expire in seven days; an 8-hour dormancy followed a skipped backstop; a mass resume hits the limit again.
ground  owner ruling 2026-10-23

### agent-messaging
when    sending to an agent or receiving a teammate's agent notification
do      address an agent by its name from `ListAgents`, read in the sending turn; relay a teammate's background agent's notification to the teammate by SendMessage
never   address an agent by a six-hex ref or a name read earlier
why     Names change on rename; a bare ref is refused.
ground  owner ruling 2026-10-23

### subagent-binding
when    dispatching any agent, or receiving its first message
do      open every brief with the binding order and a Stack block naming every element the agent binds to, each with its absolute source path, and the oath skill's path — the lead names them from .manifestos.yaml and may add elements per brief; the agent looks nothing up: it reads the oath skill and each element's source whole, binds, then SendMessages the lead one commitment line per element with each source's line count checked against wc -l, before reading any other file; the lead reads that message against the Stack block and stops and relaunches an agent whose binding misses an element, reports a partial read, or carries no commitment
never   an agent reading a brief input, a source file, or a spec before its binding message is sent; a dispatch that relies on a hook to inject the stack; work accepted from an agent whose binding was refused or never arrived
why     The hook injecting manifesto stacks may be inactive; the lead is bound by its instructions for life, a subagent is bound only by what it reads at init.
ground  owner ruling 2026-10-24T10:38

## owner

### owner-away
when    the owner is away, a decision is his, or a standing rule stands in the way
do      act as project owner: install tools, manage dependencies, create files; hesitate only on a truly dangerous operation (force push, dropping data); resolve a decision for the absent owner, or ambiguity in implementable work, under the records in the same turn; record it as the operator's decision with its ground and what could invalidate it; treat every standing rule as binding
never   park work pending a ruling; propose breaking a standing rule, even as an option
why     The owner is absent; a reversible choice costs seconds to overturn; a rule offered for waiver has stopped ruling.
ground  owner ruling 2026-10-23
