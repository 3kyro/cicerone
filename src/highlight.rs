use crate::syntax::Lang;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hl {
    Keyword,
    Function,
    Type,
    Constructor,
    String,
    Number,
    Comment,
    Operator,
    Module,
    Param,
    Punct,
}

/// Highlight spans per line (0-based), as byte ranges within the line.
pub type Spans = Vec<Vec<(usize, usize, Hl)>>;

fn query(lang: Lang) -> Option<&'static Query> {
    static HS: OnceLock<Option<Query>> = OnceLock::new();
    static ELM: OnceLock<Option<Query>> = OnceLock::new();
    let (cell, src) = match lang {
        Lang::Haskell => (&HS, tree_sitter_haskell::HIGHLIGHTS_QUERY),
        Lang::Elm => (&ELM, tree_sitter_elm::HIGHLIGHTS_QUERY),
    };
    cell.get_or_init(|| Query::new(&lang.grammar(), src).ok()).as_ref()
}

fn class(capture: &str) -> Option<Hl> {
    let head = capture.split('.').next().unwrap_or(capture);
    Some(match (head, capture) {
        (_, c) if c.starts_with("variable.parameter") => Hl::Param,
        (_, c) if c.starts_with("storage.type") => Hl::Type,
        (_, c) if c.starts_with("keyword.operator") => Hl::Operator,
        (_, c) if c.starts_with("keyword.type") => Hl::Keyword,
        ("keyword" | "storage" | "conditional" | "repeat" | "include" | "boolean", _) => Hl::Keyword,
        ("function" | "method" | "local", _) => Hl::Function,
        ("type" | "union", _) => Hl::Type,
        ("constructor", _) => Hl::Constructor,
        ("string" | "char", _) => Hl::String,
        ("number" | "float", _) => Hl::Number,
        ("comment", _) => Hl::Comment,
        ("operator", _) => Hl::Operator,
        ("module" | "namespace" | "meta", _) => Hl::Module,
        ("punctuation", _) => Hl::Punct,
        _ => return None,
    })
}

pub fn warm() {
    query(Lang::Haskell);
    query(Lang::Elm);
}

pub fn highlight(lang: Lang, src: &str) -> Spans {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(src.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    let mut per_byte: Vec<Option<Hl>> = vec![None; src.len()];
    if let Some(q) = query(lang) {
        let mut parser = Parser::new();
        if parser.set_language(&lang.grammar()).is_ok()
            && let Some(tree) = parser.parse(src, None)
        {
            let names = q.capture_names();
            let mut cursor = QueryCursor::new();
            let mut caps = cursor.captures(q, tree.root_node(), src.as_bytes());
            while let Some((m, i)) = caps.next() {
                let c = m.captures()[*i];
                if let Some(hl) = class(names[c.index as usize]) {
                    per_byte[c.node.byte_range()].fill(Some(hl));
                }
            }
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let e = starts.get(n + 1).map_or(src.len(), |x| x - 1);
            let mut spans = Vec::new();
            let mut i = s;
            while i < e {
                let cur = per_byte[i];
                let j = (i..e).find(|&k| per_byte[k] != cur).unwrap_or(e);
                if let Some(h) = cur {
                    spans.push((i - s, j - s, h));
                }
                i = j;
            }
            spans
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_haskell_and_elm() {
        let hs = highlight(Lang::Haskell, "f :: Int -> Int\nf x = case x of\n  0 -> 1\n");
        assert!(hs[0].iter().any(|&(_, _, h)| h == Hl::Type));
        assert!(hs[1].iter().any(|&(_, _, h)| h == Hl::Keyword));
        let elm = highlight(Lang::Elm, "module A exposing (f)\n\nf : Int -> Int\nf x =\n    x + 1\n");
        assert!(elm[2].iter().any(|&(_, _, h)| h == Hl::Type), "{:?}", elm[2]);
    }
}
