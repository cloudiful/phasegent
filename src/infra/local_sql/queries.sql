-- Local provider queries (issue 211 P2).
-- Single source for LocalProvider SQLite statements; loaded via
-- include_str! and selected by `-- name:` headers. No DDL here;
-- schema/seeds stay in schema.sql/seed.sql. Placeholders are rusqlite
-- `?N` style; callers bind state/query/limit/offset explicitly.

-- name: get_issue_by_id
SELECT id, title, body, status, project, tracker, author_role, created_at, updated_at, closed_at
FROM local_issues WHERE id = ?1;

-- name: count_issues
SELECT COUNT(*) FROM local_issues
WHERE (?1 = 'all' OR (?1 = 'open' AND status NOT IN ('Closed', 'Cancelled')) OR (?1 = 'closed' AND status IN ('Closed', 'Cancelled')))
AND (?2 = '' OR instr(lower(title), lower(?2)) > 0 OR instr(lower(body), lower(?2)) > 0);

-- name: search_issues
SELECT id, title, body, status, project, tracker, author_role, created_at, updated_at, closed_at
FROM local_issues
WHERE (?1 = 'all' OR (?1 = 'open' AND status NOT IN ('Closed', 'Cancelled')) OR (?1 = 'closed' AND status IN ('Closed', 'Cancelled')))
AND (?2 = '' OR instr(lower(title), lower(?2)) > 0 OR instr(lower(body), lower(?2)) > 0)
ORDER BY id ASC LIMIT ?3 OFFSET ?4;

-- name: insert_issue
INSERT INTO local_issues (title, body, status, project, tracker, author_role, created_at, updated_at, closed_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL);

-- name: update_issue_body
UPDATE local_issues SET body = ?1, updated_at = ?2 WHERE id = ?3;

-- name: close_issue
UPDATE local_issues SET status = 'Closed', updated_at = ?1, closed_at = ?2 WHERE id = ?3;

-- name: insert_comment
INSERT INTO local_comments (issue_id, role, phase, attempt, marker, body, created_at)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);

-- name: get_comment
SELECT id, issue_id, role, phase, attempt, marker, body, created_at
FROM local_comments WHERE id = ?1 AND issue_id = ?2;

-- name: find_comment_by_marker
SELECT id, issue_id, role, phase, attempt, marker, body, created_at
FROM local_comments WHERE issue_id = ?1 AND marker = ?2;

-- name: list_projects
SELECT rowid, project, description FROM local_projects ORDER BY project ASC;

-- name: insert_project
INSERT INTO local_projects (project, description, created_at) VALUES (?1, ?2, ?3);
