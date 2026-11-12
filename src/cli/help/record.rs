use crate::policy::Role;

/// The `record` group is gated by role rather than by one capability row,
/// so its pages read the same role lists the registry and the execution
/// gate use. A role-less view stays the full superset.
fn may_read(role: Option<Role>) -> bool {
    role.is_none_or(|role| Role::RECORD_READ_ROLES.contains(&role))
}

pub(crate) fn print_record_help(role: Option<Role>) {
    println!(
        "Structured agent record commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| Role::RECORD_WRITE_ROLES.contains(&role)) {
        println!(
            "  {:<14} Publish one record with a CLI-owned metadata header",
            "create"
        );
    }
    if may_read(role) {
        println!("  {:<14} Read one record by its native reference id", "get");
        println!(
            "  {:<14} List records on an issue, optionally filtered",
            "list"
        );
    }
    println!("\nUse 'phasegent --help record <command>' for options.");
}

pub(crate) fn print_record_command_help(role: Option<Role>, command: &str) {
    // A page is only rendered for a role the command admits: the
    // registry gate already denies the command, so this keeps a denied
    // page from leaking its parameters.
    let admitted = match command {
        "create" => role.is_none_or(|role| Role::RECORD_WRITE_ROLES.contains(&role)),
        "get" | "list" => may_read(role),
        _ => {
            print_record_help(role);
            return;
        }
    };
    if !admitted {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
        return;
    }
    let text = match command {
        "create" => create_help(),
        "get" => {
            "Usage: record get <ISSUE> <RECORD_ID>\n\nReads one record by the native reference id a create returned, as the plain note body plus its structured fields. The generated header is never shown. The id is the native reference (`#change-<id>` on Redmine, `#note-<id>` locally), so a parent cites the record id instead of transcribing the note.\n\nA comment that opens with the reserved record header but is not a valid record is returned as an invalid entry instead of an error: the same `status=invalid` object a listing shows, with the stored body as `raw` and the decode failure as `error`. The read still succeeds, and the invalid entry carries no kind, actor, key, phase, attempt, review, recon, or body: invalid content is never record metadata. An ordinary comment is not a record and stays a not-found result."
        }
        "list" => {
            "Usage: record list <ISSUE> [--kind KIND] [--phase TOKEN] [--recon TOKEN] [--page N] [--limit N]\n\nReturns `{items,page,limit,total_count,has_more,issue,invalid_count}` with 50 structured records and their plain note bodies by default (page 1, limit 50); --page is positive and 1-based, and --limit is 1..100. total_count is exact after filtering. Filters are matched locally and exactly; ordinary comments are not records and are not listed. Pagination is output-only: the complete comment collection is decoded and filtered before slicing; Redmine journal transfer is not reduced. See docs/cli-pagination.md. A tracked explorer publishes an authorized recon record and returns only its record id/url/key/provider/issue; the findings live in the record body.\n\nEvery item carries a `status`: `record` for a valid record, which keeps all of its structured fields, and `invalid` for a comment whose reserved header is not a valid record. An invalid item shows only its id, link, issue, the stored body as `raw`, and the decode failure as `error`; it never carries record metadata. Invalid items are always listed, whatever the filters are, while valid items obey them, and `invalid_count` counts the invalid entries of the whole filtered collection even when they are on another page. A short warning on stderr names those entries."
        }
        _ => return,
    };
    println!("{text}");
}

/// The create page spells out the field rules and the retry contract,
/// because those are what an agent has to get right without a chat
/// transcript. The kind is derived from the verified role, so the page
/// never offers a kind flag.
fn create_help() -> &'static str {
    "Usage: record create <ISSUE> --key TOKEN [--phase TOKEN --attempt POSITIVE] [--review final|checkpoint] [--recon TOKEN] (--body TEXT | --body-file PATH [--keep-body-file]) [--authorized]\n\nThe kind is derived from the verified role: an executor publishes an executor record, a reviewer a reviewer record, and a recon role a recon record. There is no kind flag and no actor override, so an agent can never label its own work as another role's. An executor record requires --phase and --attempt, a reviewer record additionally requires --review, and a recon record requires --recon and rejects --phase, --attempt, and --review. An impossible combination is rejected before any provider access or body-file read.\n\n--key is a stable request token (1-128 characters from [A-Za-z0-9._:-]). Reuse it only to retry one identical logical request: a retry returns the existing record, while a different body or metadata under a used key is a conflict. A new attempt needs a new key. This is not atomic deduplication, so concurrent requests claiming one key are reported as an error rather than guessed.\n\nThe CLI generates the versioned metadata header; supply only metadata and the plain note body. The returned id is the native reference (`#change-<id>` on Redmine, `#note-<id>` locally): a parent cites the record id in the issue plan instead of transcribing the note. Ordinary comment commands stay available for historical records and the existing audit protocol, unchanged.\n\nA stored comment whose reserved header is not a valid record never blocks a new key: it is tolerated when its own request key is a different, established one. An invalid record that claims the requested key, or whose key cannot be established at all, refuses the write before anything is written, and a warning names the tolerated entries.\n\n--body-file follows the comment flow: read and validated locally before any provider or network access, deleted after a confirmed write unless --keep-body-file is given, and kept on any failure.\n\nValues beginning with `-` must use the inline form: --body=TEXT or --key=TOKEN."
}
