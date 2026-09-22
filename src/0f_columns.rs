/// Walks an internal SQL fragment and visits every bare `c<digits>` column
/// reference outside quoted strings and identifiers.
fn visit_columns(sql: &str, mut visit: impl FnMut(&str, usize)) {
    let bytes = sql.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' || b == b'"' || b == b'`' || b == b'[' {
            let close = if b == b'[' { b']' } else { b };
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == close {
                    if close != b']' && bytes.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
            i += 1;
            visit(&sql[start..i.min(bytes.len())], usize::MAX);
            continue;
        }
        let word = b.is_ascii_alphanumeric() || b == b'_';
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if word {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let token = &sql[start..i];
            let column = boundary
                .then(|| token.strip_prefix('c'))
                .flatten()
                .filter(|d| !d.is_empty() && d.bytes().all(|x| x.is_ascii_digit()))
                .and_then(|d| d.parse().ok());
            visit(token, column.unwrap_or(usize::MAX));
            continue;
        }
        visit(&sql[i..i + 1], usize::MAX);
        i += 1;
    }
}
pub(crate) fn column_references(sql: &str) -> Vec<usize> {
    let mut out = vec![];
    visit_columns(sql, |_, c| {
        if c != usize::MAX {
            out.push(c)
        }
    });
    out
}
pub(crate) fn substitute_columns(sql: &str, expression: impl Fn(usize) -> String) -> String {
    let mut out = String::with_capacity(sql.len());
    visit_columns(sql, |token, c| {
        if c == usize::MAX { out.push_str(token); }
        else { out.push_str(&expression(c)); }
    });
    out
}
pub(crate) fn renumber_columns(sql: &str, map: impl Fn(usize) -> usize) -> String {
    let mut out = String::with_capacity(sql.len());
    visit_columns(sql, |token, c| {
        if c == usize::MAX {
            out.push_str(token)
        } else {
            out.push_str(&format!("c{}", map(c)))
        }
    });
    out
}
