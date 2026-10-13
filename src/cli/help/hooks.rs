pub(crate) fn print_hooks_help() {
    println!(
        "Managed Git hook commands:\n\n  install          Install/update prepare-commit-msg and commit-msg\n\nUse 'phasegent --help hooks install' for details."
    );
}

pub(crate) fn print_hooks_command_help(command: &str) {
    if command == "run" {
        println!(
            "Usage: hooks run prepare-commit-msg <message-file> [source [commit-object]]\n       hooks run commit-msg <message-file>\n\nThe third prepare argument is accepted only for source=commit; it is opaque. Git-generated commit/merge/squash sources are not rewritten. Hook calls need no role or credentials."
        );
    } else if command == "install" {
        println!(
            "Usage: hooks install\n\nInstalls/updates wrappers in Git's effective hooks directory (including core.hooksPath), discovered with git rev-parse --git-path hooks. Foreign hooks are renamed into phasegent-original/<hook-name> in that directory and run first with unchanged arguments; failures stop the chain. Missing or unsafe preserved originals block execution and reinstall: restore the regular executable original or manually reconcile the wrapper first.\n\nIssue context comes from durable local branch links (issue bind), with a branch-name fallback; default/detached/unbound branches are no-ops. Bound hooks require fixed core.commentChar/core.commentString, not auto. After updating the PATH binary, run hooks install in each effective hooks directory to update wrappers. --no-verify skips commit-msg, not prepare-commit-msg. See docs/git-hooks.md."
        );
    } else {
        print_hooks_help();
    }
}
