pub(crate) fn print_doctor_help() {
    println!(
        "Usage: phasegent doctor\n\nRead-only self-check for operators and agents (no --role required). Reports credential presence per role and provider (fingerprint plus store time, never values), the resolved issue-index backend, and the masked PostgreSQL URL (host/database only; userinfo, query, and fragment stripped). Takes no arguments.\n\nUse doctor instead of schema dumps, substr(credential, ...) peeks, or raw global_setting reads: it answers \"which key / which backend\" through the one approved channel."
    );
}
