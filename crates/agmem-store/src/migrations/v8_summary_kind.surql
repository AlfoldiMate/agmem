-- v8 (issue #85): the `summary` kind — a digest written by `reflect` whose
-- `derived_from` children expand on demand. Widening the ASSERT only accepts
-- more, so no UPDATE rides along and v3's re-coercion trap does not apply.
DEFINE FIELD OVERWRITE kind ON memory TYPE string
    ASSERT $value IN ["fact", "lesson", "instruction", "summary"];
