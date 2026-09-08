-- Local provider SQLite seeds (issue 211 P4).
-- Idempotent: INSERT OR IGNORE so re-open never duplicates.
-- Status edges mirror redmine STATUS_TRANSITIONS exactly; the Redmine
-- installation workflow stays authoritative at runtime (policy guidance
-- only). Closed/Cancelled are terminal and contribute no rows.

INSERT OR IGNORE INTO local_projects (project, description, created_at)
    VALUES ('default', 'Default local project', 1700000000);

INSERT OR IGNORE INTO status_transitions (from_status, to_status) VALUES
    ('New', 'In Progress'),
    ('New', 'Cancelled'),
    ('In Progress', 'In Review'),
    ('In Progress', 'Blocked'),
    ('In Progress', 'Cancelled'),
    ('In Review', 'Resolved'),
    ('In Review', 'Changes Requested'),
    ('In Review', 'Blocked'),
    ('In Review', 'Cancelled'),
    ('Changes Requested', 'In Progress'),
    ('Changes Requested', 'Blocked'),
    ('Changes Requested', 'Cancelled'),
    ('Blocked', 'In Progress'),
    ('Blocked', 'Cancelled'),
    ('Resolved', 'In Progress'),
    ('Resolved', 'Closed');
