//! Branch-name parsing for active-issue resolution.
//!
//! A branch whose name carries an issue id resolves without any durable
//! link, so the worktree naming convention (`<type>/<id>`, e.g.
//! `feat/452`) and the trailing-id shapes stay readable. This is
//! the only supported fallback: nothing here reads Git config, storage,
//! or the network.
//!
//! Precedence (first match wins, strictly positive ids only):
//! 1. `type/id[-slug]` — the segment after `/` starts with digits
//!    (e.g. `feat/452` -> 452, `feat/452-slug` -> 452).
//! 2. Trailing `-`/`_` id (e.g. `feat/help-unify-447` -> 447).
//! 3. Bare digits (e.g. `452` -> 452).

/// Parse the issue id encoded in `branch`, or `None` when the name carries
/// no positive numeric identity.
pub fn parse_issue_id_from_branch_name(branch: &str) -> Option<u64> {
    for (index, _) in branch.match_indices('/') {
        let rest = &branch[index + 1..];
        let digit_prefix: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digit_prefix.is_empty()
            && let Ok(id) = digit_prefix.parse::<u64>()
            && id > 0
        {
            return Some(id);
        }
    }
    if let Some(pos) = branch.rfind(['-', '_'])
        && pos + 1 < branch.len()
    {
        let tail = &branch[pos + 1..];
        if !tail.is_empty()
            && tail.chars().all(|c| c.is_ascii_digit())
            && let Ok(id) = tail.parse::<u64>()
            && id > 0
        {
            return Some(id);
        }
    }
    if !branch.is_empty()
        && branch.chars().all(|c| c.is_ascii_digit())
        && let Ok(id) = branch.parse::<u64>()
        && id > 0
    {
        return Some(id);
    }
    None
}
