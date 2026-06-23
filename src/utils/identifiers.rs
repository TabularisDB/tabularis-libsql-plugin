//! SQL identifier quoting. libSQL/SQLite uses ANSI double quotes.

/// Quote an identifier with `"`, doubling any embedded double quotes to escape
/// them, exactly as SQLite expects.
///
/// ```
/// use libsql_plugin::utils::identifiers::quote;
/// assert_eq!(quote("users"), "\"users\"");
/// assert_eq!(quote("weird\"name"), "\"weird\"\"name\"");
/// ```
pub fn quote(name: &str) -> String {
    quote_with(name, '"')
}

/// Quote with an explicit quote character (kept generic for testing).
pub fn quote_with(name: &str, q: char) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push(q);
    for c in name.chars() {
        if c == q {
            out.push(q);
        }
        out.push(c);
    }
    out.push(q);
    out
}

/// Escape a string literal for use inside single quotes in SQL.
pub fn quote_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_plain_names() {
        assert_eq!(quote("users"), "\"users\"");
    }

    #[test]
    fn escapes_embedded_quotes() {
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
        assert_eq!(quote_with("a`b", '`'), "`a``b`");
    }

    #[test]
    fn handles_empty() {
        assert_eq!(quote(""), "\"\"");
    }

    #[test]
    fn escapes_string_literals() {
        assert_eq!(quote_literal("O'Brien"), "'O''Brien'");
        assert_eq!(quote_literal("plain"), "'plain'");
    }
}
