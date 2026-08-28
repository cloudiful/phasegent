pub(crate) fn print_hooks_help() {
    println!(
        "Managed Git hook commands:\n\n  install          Install/update the managed prepare-commit-msg and commit-msg hooks\n\nUse 'phasegent --help hooks install' for options."
    );
}

pub(crate) fn print_hooks_command_help(command: &str) {
    if command != "install" {
        print_hooks_help();
        return;
    }
    println!(
        "Usage: hooks install\n\nInstalls or updates the managed prepare-commit-msg and commit-msg hooks in the current checkout's Git hooks directory. Existing unrelated hooks are preserved: they are moved to .git/hooks/phasegent-original/<hook-name> and chained so the original runs first. Managed hooks call `phasegent hooks run ...` locally and need no credentials; issue references come from the current branch's local Git config binding (`issue bind`)."
    );
}
