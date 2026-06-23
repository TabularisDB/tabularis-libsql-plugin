//! Lightweight SQL statement classification.
//!
//! libSQL accepts arbitrary SQL in `execute_query`, so we need to tell apart
//! statements that return rows (and can be paginated) from statements that
//! only report an affected-row count.

/// Strip a leading line/block comment and surrounding whitespace, then return
/// the first SQL keyword in lowercase.
pub fn first_keyword(sql: &str) -> String {
    let trimmed = sql.trim_start();
    trimmed
        .split(|c: char| c.is_whitespace() || c == '(')
        .find(|tok| !tok.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Does this statement produce a result set we should read with a row cursor?
pub fn returns_rows(sql: &str) -> bool {
    matches!(
        first_keyword(sql).as_str(),
        "select" | "with" | "pragma" | "explain" | "values"
    )
}

/// Can we safely wrap this statement in `SELECT * FROM (...)` for LIMIT/OFFSET
/// pagination and `SELECT COUNT(*) FROM (...)` counting? Only true SELECT-shaped
/// statements qualify — PRAGMA/EXPLAIN cannot be used as subqueries.
pub fn is_wrappable(sql: &str) -> bool {
    matches!(first_keyword(sql).as_str(), "select" | "with")
}

/// Trim trailing semicolons and whitespace so a statement can be embedded as a
/// subquery without a syntax error.
pub fn strip_trailing_semicolons(sql: &str) -> &str {
    sql.trim().trim_end_matches(';').trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_first_keyword_case_insensitively() {
        assert_eq!(first_keyword("  SELECT 1"), "select");
        assert_eq!(first_keyword("\nWITH x AS (...)"), "with");
        assert_eq!(first_keyword("INSERT INTO t VALUES(1)"), "insert");
        assert_eq!(first_keyword("(select 1)"), "select");
    }

    #[test]
    fn classifies_row_returning_statements() {
        assert!(returns_rows("select * from t"));
        assert!(returns_rows("PRAGMA table_info(t)"));
        assert!(returns_rows("EXPLAIN QUERY PLAN select 1"));
        assert!(!returns_rows("insert into t values (1)"));
        assert!(!returns_rows("update t set a = 1"));
        assert!(!returns_rows("create table t (id integer)"));
    }

    #[test]
    fn only_selects_are_wrappable() {
        assert!(is_wrappable("select 1"));
        assert!(is_wrappable("WITH a AS (select 1) select * from a"));
        assert!(!is_wrappable("pragma table_info(t)"));
        assert!(!is_wrappable("insert into t values (1)"));
    }

    #[test]
    fn strips_trailing_semicolons() {
        assert_eq!(strip_trailing_semicolons("select 1; "), "select 1");
        assert_eq!(strip_trailing_semicolons("select 1 ;;"), "select 1");
    }
}
