# Native Git hooks

Run `phasegent hooks install` inside a checkout after putting the updated binary on
`PATH`. Repeat installation after binary updates to refresh wrappers. Git's
`core.hooksPath` and shared worktree hooks directory are honored; preserved
originals live in `phasegent-original/<hook-name>` under the effective directory.

Foreign regular executable hooks are renamed without changing their bytes or
mode. The original runs first with all Git arguments unchanged; its nonzero exit
stops the chain. Known managed wrappers update in place, and repeated installs
are stable. Symlinks and backup collisions are refused. If a declared original
is missing, nonregular, or nonexecutable, execution and reinstall fail closed.
Restore the original as a regular executable file, or explicitly reconcile/remove
or replace the managed wrapper manually. Reinstallation alone does not repair a
missing original.

Hooks use durable local branch links (`issue bind`, one or more ids per call)
with a branch-name fallback. Default, detached, and unbound branches are no-ops,
and hook calls require no credentials or role. A branch bound to several issues
resolves as one deterministic set: `commit-msg` accepts references to any member
and rejects references outside the set, and a number linked under more than one
provider/project scope fails closed instead of guessing. Prepare runs before the
editor; commit-msg validates after the editor, before Git's final cleanup.
Git-generated commit, merge, and squash prepare sources are left unchanged.
Native amend, `-c`, and `-C` calls accept Git's opaque third commit argument:

```
phasegent hooks run prepare-commit-msg <message-file> [source [commit-object]]
phasegent hooks run commit-msg <message-file>
```

The third argument is valid only with source `commit`; malformed counts and
leading-dash arguments are rejected.

Bound hooks read effective ordered `core.commentChar` / `core.commentString`
configuration, including inherited `git -c` overrides; the last alias wins.
Absent configuration defaults to `#`. Select a fixed marker: `auto` is rejected
for bound message work without modifying the file, because Git's chosen marker
cannot reliably be observed. Skipped prepare sources and unbound branches do
not require marker configuration.

Issue checks ignore column-zero comment-prefixed lines and content below the
exact configured-marker Git scissors delimiter. Indented authored text remains
subject to checks. The six reference keywords (`refs`, `references`, `ref`,
`fixes`, `closes`, `closed`) are case-insensitive, accept an optional colon, and
require a positive numeric `#N` with word boundaries. References outside the
bound set and duplicate exact generated `Refs #N` trailers for any bound issue
are rejected.

Prepare adds one `Refs #N` line per bound issue, in ascending id order, only when
the authored region has no issue reference, before scissors or otherwise at EOF.
Raw bytes, signoffs and other trailers, comment scaffold, and scissors tail are
preserved. Insertions use predominant CRLF/LF endings (ties use the first
observed ending), removing only trailing line terminators in the insertion
region. Message content is never printed.

`git commit --no-verify` skips commit-msg but **does not skip prepare-commit-msg**.
It is not a recovery mechanism for broken preserved originals or `auto` markers.
