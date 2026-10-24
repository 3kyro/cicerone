use serde::Serialize;
use std::collections::BTreeSet;
use tree_sitter::{Node, Parser, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Haskell,
    Elm,
}

impl Lang {
    pub fn of_path(path: &str) -> Option<Lang> {
        match path.rsplit_once('.')?.1 {
            "hs" | "lhs" | "hs-boot" => Some(Lang::Haskell),
            "elm" => Some(Lang::Elm),
            _ => None,
        }
    }

    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            Lang::Haskell => tree_sitter_haskell::LANGUAGE.into(),
            Lang::Elm => tree_sitter_elm::LANGUAGE.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeclKind {
    Type,
    TypeAlias,
    Class,
    Instance,
    Function,
    Port,
    Other,
}

impl DeclKind {
    pub fn is_type_level(self) -> bool {
        matches!(
            self,
            DeclKind::Type | DeclKind::TypeAlias | DeclKind::Class | DeclKind::Instance
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    Header,
    Imports,
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub kind: DeclKind,
    pub name: String,
    /// 1-based, inclusive. Covers attached doc comments.
    pub start: usize,
    pub end: usize,
    pub signature: Option<String>,
    pub sig_lines: Option<(usize, usize)>,
    pub text: String,
    pub refs: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct Outline {
    pub module: Option<String>,
    pub imports: BTreeSet<String>,
    pub decls: Vec<Decl>,
    pub regions: Vec<(Region, usize, usize)>,
}

impl Outline {
    pub fn decls_at(&self, line: usize) -> impl Iterator<Item = usize> + '_ {
        self.decls
            .iter()
            .enumerate()
            .filter(move |(_, d)| d.start <= line && line <= d.end)
            .map(|(i, _)| i)
    }

    pub fn region_at(&self, line: usize) -> Option<Region> {
        self.regions
            .iter()
            .find(|(_, s, e)| *s <= line && line <= *e)
            .map(|(r, _, _)| *r)
    }
}

pub fn outline(lang: Lang, src: &str) -> Outline {
    let mut parser = Parser::new();
    if parser.set_language(&lang.grammar()).is_err() {
        return Outline::default();
    }
    let Some(tree) = parser.parse(src, None) else {
        return Outline::default();
    };
    let root = tree.root_node();
    let mut out = Outline::default();
    let mut items: Vec<Node> = Vec::new();
    match lang {
        Lang::Haskell => {
            for child in named_children(root) {
                match child.kind() {
                    "header" => {
                        out.module = child
                            .child_by_field_name("module")
                            .map(|n| text(n, src).to_string());
                        out.regions.push(region(Region::Header, child));
                        items.push(child);
                    }
"imports" => {
                        let (imports, trailing): (Vec<Node>, Vec<Node>) = named_children(child)
                            .into_iter()
                            .partition(|c| c.kind() == "import");
                        out.imports.extend(
                            imports
                                .iter()
                                .filter_map(|i| i.child_by_field_name("module"))
                                .map(|n| text(n, src).to_string()),
                        );
                        if let (Some(first), Some(last)) = (imports.first(), imports.last()) {
                            out.regions.push((
                                Region::Imports,
                                line_of(first.start_position().row),
                                line_of(last.end_position().row),
                            ));
                        }
                        items.push(child);
                        items.extend(trailing);
                    }
                    "declarations" => items.extend(named_children(child)),
                    _ => items.push(child),
                }
            }
        }
        Lang::Elm => {
            for child in named_children(root) {
                match child.kind() {
                    "module_declaration" => {
                        out.module = qid(child, src);
                        out.regions.push(region(Region::Header, child));
                    }
                    "import_clause" => {
                        out.imports.extend(qid(child, src));
                        out.regions.push(region(Region::Imports, child));
                    }
                    _ => {}
                }
                items.push(child);
            }
        }
    }
    let mut doc_start: Option<usize> = None;
    for node in items {
        if is_doc(lang, node, src) {
            doc_start.get_or_insert(line_of(node.start_position().row));
            continue;
        }
        if is_comment(node) {
            doc_start = None;
            continue;
        }
        let Some((kind, name, is_sig)) = classify(lang, node, src) else {
            doc_start = None;
            continue;
        };
        let start = doc_start.take().unwrap_or(line_of(node.start_position().row));
        let end = line_of(node.end_position().row);
        let text = text(node, src).to_string();
        let mut refs = BTreeSet::new();
        collect_refs(node, src, &mut refs);
        if let Some(prev) = out.decls.last_mut()
            && prev.name == name
            && prev.kind == kind
            && prev.end < start
        {
            prev.end = end;
            prev.text.push('\n');
            prev.text.push_str(&text);
            prev.refs.extend(refs);
            if is_sig {
                prev.signature = Some(text);
                prev.sig_lines = Some((line_of(node.start_position().row), end));
            }
            continue;
        }
        out.decls.push(Decl {
            kind,
            name,
            start,
            end,
            signature: is_sig.then(|| text.clone()),
            sig_lines: is_sig.then(|| (line_of(node.start_position().row), end)),
            text,
            refs,
        });
    }
    for d in &mut out.decls {
        d.refs.remove(&d.name);
    }
    out
}

pub fn parse(lang: Lang, src: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&lang.grammar()).ok()?;
    parser.parse(src, None)
}

/// Lines in `start..=end` that carry the control flow of a declaration:
/// equations, guards, case branches, if branches, local bindings, and do
/// statements. A skeleton shows these lines and folds the rest.
pub fn anchors(lang: Lang, tree: &Tree, start: usize, end: usize) -> BTreeSet<usize> {
    let mut out = BTreeSet::new();
    let (structural, branching, keywords): (&[&str], &[&str], &[&str]) = match lang {
        Lang::Haskell => (
            &[
                "signature", "function", "bind", "match", "guards", "alternative", "statement",
                "local_binds", "let", "let_in", "multi_way_if", "lambda_case", "lambda_cases",
            ],
            &["conditional"],
            &["where", "in", "then", "else", "of"],
        ),
        Lang::Elm => (
            &[
                "type_annotation", "value_declaration", "case_of_branch", "let_in_expr",
                "case_of_expr",
            ],
            &["if_else_expr"],
            &["in", "then", "else", "of"],
        ),
    };
    let mut stack = vec![(tree.root_node(), false)];
    while let Some((node, in_list)) = stack.pop() {
        let (s, e) = (line_of(node.start_position().row), line_of(node.end_position().row));
        if e < start || s > end {
            continue;
        }
        let kind = node.kind();
        if structural.contains(&kind) || (!node.is_named() && keywords.contains(&kind)) {
            out.insert(s);
        }
        if branching.contains(&kind) {
            out.insert(s);
            for c in named_children(node) {
                out.insert(line_of(c.start_position().row));
            }
        }
        let list = lang == Lang::Elm && kind == "list_expr";
        if list && !in_list {
            for c in named_children(node) {
                out.insert(line_of(c.start_position().row));
            }
        }
        if matches!(kind, "signature" | "type_annotation") {
            continue;
        }
        let mut c = node.walk();
        stack.extend(node.children(&mut c).map(|c| (c, in_list || list)));
    }
    out.retain(|l| (start..=end).contains(l));
    out
}

fn classify(lang: Lang, node: Node, src: &str) -> Option<(DeclKind, String, bool)> {
    let name_field = || node.child_by_field_name("name").map(|n| text(n, src).to_string());
    let first_ident = |kinds: &[&str]| {
        named_children(node)
            .into_iter()
            .find(|c| kinds.contains(&c.kind()))
            .map(|c| text(c, src).to_string())
    };
    let r = match (lang, node.kind()) {
        (Lang::Haskell, "signature") => (
            DeclKind::Function,
            name_field().or_else(|| first_ident(&["variable", "prefix_id", "binding_list"]))?,
            true,
        ),
        (Lang::Haskell, "function" | "bind") => (
            DeclKind::Function,
            name_field().or_else(|| first_ident(&["variable", "prefix_id"]))?,
            false,
        ),
        (Lang::Haskell, "data_type" | "newtype" | "type_family" | "data_family") => {
            (DeclKind::Type, name_field()?, false)
        }
        (Lang::Haskell, "type_synomym") => (DeclKind::TypeAlias, name_field()?, false),
        (Lang::Haskell, "class") => (DeclKind::Class, name_field()?, false),
        (Lang::Haskell, "instance" | "deriving_instance") => {
            (DeclKind::Instance, head(text(node, src)), false)
        }
        (Lang::Haskell, "pattern_synonym") => (DeclKind::Other, head(text(node, src)), false),
        (Lang::Elm, "type_annotation") => (DeclKind::Function, name_field()?, true),
        (Lang::Elm, "value_declaration") => {
            let left = named_children(node)
                .into_iter()
                .find(|c| c.kind() == "function_declaration_left")?;
            let name = named_children(left).first().map(|n| text(*n, src).to_string())?;
            (DeclKind::Function, name, false)
        }
        (Lang::Elm, "type_declaration") => (DeclKind::Type, name_field()?, false),
        (Lang::Elm, "type_alias_declaration") => (DeclKind::TypeAlias, name_field()?, false),
        (Lang::Elm, "port_annotation") => (DeclKind::Port, name_field()?, true),
        _ => return None,
    };
    Some(r)
}

fn head(s: &str) -> String {
    let h = s.split(" where").next().unwrap_or(s);
    normalize(h)
}

pub fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_doc(lang: Lang, node: Node, src: &str) -> bool {
    match lang {
        Lang::Haskell => node.kind() == "haddock",
        Lang::Elm => node.kind() == "block_comment" && text(node, src).starts_with("{-|"),
    }
}

fn is_comment(node: Node) -> bool {
    matches!(node.kind(), "comment" | "line_comment" | "block_comment" | "pragma" | "cpp")
}

fn collect_refs(node: Node, src: &str, out: &mut BTreeSet<String>) {
    if matches!(
        node.kind(),
        "variable" | "name" | "constructor" | "lower_case_identifier" | "upper_case_identifier"
    ) {
        out.insert(text(node, src).to_string());
        return;
    }
    for c in named_children(node) {
        collect_refs(c, src, out);
    }
}

fn qid(node: Node, src: &str) -> Option<String> {
    named_children(node)
        .into_iter()
        .find(|c| c.kind() == "upper_case_qid")
        .map(|n| text(n, src).to_string())
}

fn region(r: Region, node: Node) -> (Region, usize, usize) {
    (r, line_of(node.start_position().row), line_of(node.end_position().row))
}

fn named_children(node: Node) -> Vec<Node> {
    let mut c = node.walk();
    node.named_children(&mut c).collect()
}

fn text<'a>(node: Node, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

fn line_of(row: usize) -> usize {
    row + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(o: &Outline) -> Vec<(DeclKind, &str, usize, usize)> {
        o.decls
            .iter()
            .map(|d| (d.kind, d.name.as_str(), d.start, d.end))
            .collect()
    }

    #[test]
    fn haskell_outline() {
        let src = "\
module A (f) where

import Data.Map

-- | A policy.
data Policy = RequireAll | RequireAny Int

-- | Check it.
check :: Policy -> Int -> Bool
check RequireAll _ = True
check (RequireAny n) k = k >= n

-- plain comment
newtype N = N Int

instance Show Policy where
  show _ = \"p\"
";
        let o = outline(Lang::Haskell, src);
        assert_eq!(
            names(&o),
            vec![
                (DeclKind::Type, "Policy", 5, 6),
                (DeclKind::Function, "check", 8, 11),
                (DeclKind::Type, "N", 14, 14),
                (DeclKind::Instance, "instance Show Policy", 16, 17),
            ]
        );
        let check = &o.decls[1];
        assert_eq!(check.signature.as_deref(), Some("check :: Policy -> Int -> Bool"));
        assert!(check.refs.contains("RequireAny"));
        assert!(check.refs.contains("Policy"));
        assert_eq!(o.region_at(3), Some(Region::Imports));
        assert_eq!(o.region_at(1), Some(Region::Header));
        assert_eq!(o.module.as_deref(), Some("A"));
        assert!(o.imports.contains("Data.Map"));
    }

    #[test]
    fn haskell_skeleton_anchors() {
        let src = "\
check :: Policy -> Int -> Bool
check policy k =
  case policy of
    RequireAll ->
      allOf
        k
    RequireAny n
      | n <= 0 -> False
      | otherwise -> k >= n
  where
    allOf = const True
";
        let tree = parse(Lang::Haskell, src).unwrap();
        let a: Vec<usize> = anchors(Lang::Haskell, &tree, 1, 11).into_iter().collect();
        assert_eq!(a, vec![1, 2, 3, 4, 7, 8, 9, 10, 11]);
        let o = outline(Lang::Haskell, src);
        assert_eq!(o.decls[0].sig_lines, Some((1, 1)));
    }

    #[test]
    fn module_doc_does_not_attach_to_first_decl() {
        let hs = "-- | Module doc.\nmodule A where\n\nimport B\n\nx :: Int\nx = 1\n";
        assert_eq!(names(&outline(Lang::Haskell, hs)), vec![(DeclKind::Function, "x", 6, 7)]);
        let elm = "module A exposing (x)\n\n{-| Module doc.\n-}\n\nimport B\n\n\nx : Int\nx =\n    1\n";
        assert_eq!(names(&outline(Lang::Elm, elm)), vec![(DeclKind::Function, "x", 9, 11)]);
    }

    #[test]
    fn elm_outline() {
        let src = "\
module A exposing (view)

import Html exposing (text)


{-| The model.
-}
type alias Model =
    { count : Int }


type Msg
    = Inc
    | Dec


view : Model -> Html Msg
view model =
    text (String.fromInt model.count)
";
        let o = outline(Lang::Elm, src);
        assert_eq!(
            names(&o),
            vec![
                (DeclKind::TypeAlias, "Model", 6, 9),
                (DeclKind::Type, "Msg", 12, 14),
                (DeclKind::Function, "view", 17, 19),
            ]
        );
        assert_eq!(o.decls[2].signature.as_deref(), Some("view : Model -> Html Msg"));
        assert_eq!(o.module.as_deref(), Some("A"));
        assert!(o.imports.contains("Html"));
        assert!(o.decls[2].refs.contains("Model"));
        assert_eq!(o.region_at(3), Some(Region::Imports));
    }
}
