//! Region ids and how their locks exclude each other. Forks name regions (`para:<id>`,
//! `cell:<sheet>:<A1>`, `shape:<slide>:<shape>`, ...); the server only knows the special ones:
//! `*` (the whole document) and the order locks `pages`, `slides` and `sheet:<id>`.

/// The whole document.
pub const WHOLE: &str = "*";

/// Longest region id, in bytes.
pub const MAX_REGION_BYTES: usize = 256;

/// A region id a client may name: 1 to 256 bytes, no control characters, and a sheet id that is
/// not empty and holds no `:` (else `sheet:a` and `sheet:a:b` would both cover `cell:a:b:A1`).
pub fn valid(region: &str) -> bool {
    let sheet_ok = region
        .strip_prefix("sheet:")
        .is_none_or(|id| !id.is_empty() && !id.contains(':'));
    !region.is_empty()
        && region.len() <= MAX_REGION_BYTES
        && !region.chars().any(char::is_control)
        && sheet_ok
}

/// Whether locks on `a` and `b` held by two different members would clash: the same region, `*`
/// against anything, or `sheet:<id>` against a cell of that sheet (`cell:<id>:...`). `pages` and
/// `slides` clash only with themselves: whether a page or slide operation touches an annotation or
/// shape is unknowable here.
pub fn conflicts(a: &str, b: &str) -> bool {
    a == b || a == WHOLE || b == WHOLE || contains(a, b) || contains(b, a)
}

/// Whether holding `outer` makes its holder the only one who may touch `inner`: `*` covers
/// everything, `sheet:<id>` the cells of that sheet, and every region covers itself.
pub fn covers(outer: &str, inner: &str) -> bool {
    outer == inner || outer == WHOLE || contains(outer, inner)
}

/// `sheet:<id>` contains `cell:<id>:<anything>`.
fn contains(order: &str, child: &str) -> bool {
    let Some(sheet) = order.strip_prefix("sheet:") else {
        return false;
    };
    child
        .strip_prefix("cell:")
        .and_then(|rest| rest.strip_prefix(sheet))
        .is_some_and(|rest| rest.starts_with(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_ids_are_short_and_printable() {
        assert!(valid("para:1a2b"));
        assert!(valid("field:Họ và tên"));
        assert!(valid(&"x".repeat(MAX_REGION_BYTES)));
        assert!(!valid(""));
        assert!(!valid(&"x".repeat(MAX_REGION_BYTES + 1)));
        assert!(!valid("para:1\n"));
        assert!(valid("sheet:s1"));
        assert!(!valid("sheet:"), "no sheet");
        assert!(!valid("sheet:a:b"), "would nest inside sheet:a");
    }

    #[test]
    fn whole_document_clashes_with_everything() {
        for other in ["*", "para:1", "pages", "sheet:s1", "cell:s1:A1"] {
            assert!(conflicts(WHOLE, other), "{other}");
            assert!(conflicts(other, WHOLE), "{other}");
            assert!(covers(WHOLE, other), "{other}");
        }
    }

    #[test]
    fn a_sheet_lock_clashes_with_its_own_cells_only() {
        assert!(conflicts("sheet:s1", "cell:s1:A1"));
        assert!(conflicts("cell:s1:B7", "sheet:s1"));
        assert!(covers("sheet:s1", "cell:s1:A1"));
        assert!(!covers("cell:s1:A1", "sheet:s1"));
        assert!(!conflicts("sheet:s1", "cell:s10:A1"));
        assert!(!conflicts("sheet:s1", "cell:s2:A1"));
        assert!(!conflicts("sheet:s1", "sheet:s2"));
        assert!(!conflicts("sheet:s1", "cells1:A1"));
        assert!(!conflicts("sheet:s1", "cell:s1"));
    }

    #[test]
    fn page_and_slide_order_locks_clash_with_themselves_only() {
        assert!(conflicts("pages", "pages"));
        assert!(conflicts("slides", "slides"));
        for child in ["annot:N1", "field:a.b", "shape:s1:h1", "slide:s1"] {
            assert!(!conflicts("pages", child), "{child}");
            assert!(!conflicts("slides", child), "{child}");
        }
        assert!(!conflicts("para:1", "para:2"));
    }
}
