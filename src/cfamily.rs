//! Definitions in C and C++ files, by line scanning like the rest of the
//! code index, but by C's grammar: a C function has no keyword before its
//! name (`static int foo(int x) {`), so `code::definitions`'s keyword scan
//! found 3% of what `ctags` finds and took `static int` for a definition
//! named `int`.
//!
//! - `#define NAME` is a constant, `#define NAME(` a function.
//! - At file, namespace, `extern "C"`, class or struct level, a statement
//!   head ending in `{` with a parenthesis is a function: the identifier
//!   before its first `(` (`Foo::bar` is `bar`). The head may span lines
//!   (GNU style puts the return type on its own line).
//! - `struct`/`union`/`class`/`enum` with a body is a type, and so is every
//!   name a `typedef` declares. Enum constants are variants.
//! - Function bodies are skipped: locals are not definitions.
//! - Only the first branch of an `#if` is read, so a function written
//!   twice for two configurations does not unbalance the braces.

use crate::code::Def;
use crate::code::MAX_VARIANTS_PER_ENUM;

/// File extensions read as C or C++.
pub const EXTS: &[&str] = &["c", "h", "cc", "cpp", "cxx", "hpp", "hh", "hxx", "inl", "ipp", "tpp", "cu", "cuh"];

pub fn is_c_family(ext: &str) -> bool {
    EXTS.contains(&ext)
}

/// Java and C# share C's statement grammar (a method has no keyword before
/// its name either), so this scanner reads them too.
pub fn handles(ext: &str) -> bool {
    is_c_family(ext) || ext == "java" || ext == "cs"
}

const CONTROL: &[&str] = &["if", "for", "while", "switch", "return", "sizeof", "catch", "do", "else", "case", "goto", "defined", "alignof", "_Alignof", "typeof", "__typeof__", "static_assert", "_Static_assert"];
/// Names that take parentheses before the real name (`__attribute__((x)) int f(`).
const ATTRIBUTES: &[&str] = &["__attribute__", "__attribute", "__declspec", "alignas", "_Alignas", "decltype", "__asm__", "asm", "noexcept", "throw", "__pragma", "_Pragma"];
const TYPE_KW: &[&str] = &["struct", "union", "class", "enum", "interface", "record"];
/// Words before a definition that do not change what it is.
const MODS: &[&str] = &["static", "const", "volatile", "extern", "inline", "public", "private", "protected", "internal", "abstract", "final", "sealed", "partial", "readonly", "unsafe", "new", "override", "virtual", "async", "synchronized", "native", "strictfp", "transient", "default"];
const NOT_NAMES: &[&str] = &["final", "sealed", "partial", "abstract", "readonly", "internal", "interface", "record", "struct", "union", "class", "enum", "typedef", "const", "volatile", "static", "extern", "inline", "unsigned", "signed", "long", "short", "int", "char", "void", "float", "double", "public", "private", "protected", "virtual", "register", "auto", "restrict", "__restrict", "override"];

#[derive(Clone, Copy, PartialEq)]
enum Block {
    /// Definitions live here: file, namespace, `extern "C"`, class, struct.
    Container,
    /// A function body or an initializer: skipped.
    Body,
    /// An enum body: its constants are variants.
    Enum,
}

#[derive(Clone, Debug)]
struct Tok {
    text: String,
    line: usize,
}

/// Blanks comments, strings and character literals, keeping line breaks,
/// so braces and parentheses inside them do not count.
fn clean(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let b: Vec<char> = text.chars().collect();
    let (mut i, mut block) = (0usize, false);
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            out.push(std::mem::take(&mut cur));
            i += 1;
            continue;
        }
        if block {
            if c == '*' && b.get(i + 1) == Some(&'/') {
                block = false;
                cur.push_str("  ");
                i += 2;
            } else {
                cur.push(' ');
                i += 1;
            }
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'*') {
            block = true;
            cur.push_str("  ");
            i += 2;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'/') {
            // A line comment ends at the line break, unless continued.
            while i < b.len() && b[i] != '\n' {
                if b[i] == '\\' && b.get(i + 1) == Some(&'\n') {
                    out.push(std::mem::take(&mut cur));
                    i += 1;
                }
                i += 1;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            cur.push(' ');
            i += 1;
            while i < b.len() && b[i] != c && b[i] != '\n' {
                if b[i] == '\\' {
                    cur.push(' ');
                    i += 1;
                }
                cur.push(' ');
                i += 1;
            }
            cur.push(' ');
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    out.push(cur);
    out
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn tokens(line: &str, n: usize, out: &mut Vec<Tok>) {
    let b: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if is_ident_start(c) {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            out.push(Tok { text: b[s..i].iter().collect(), line: n });
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '.' || b[i] == '_') {
                i += 1;
            }
            out.push(Tok { text: "0".into(), line: n });
        } else if c == ':' && b.get(i + 1) == Some(&':') {
            out.push(Tok { text: "::".into(), line: n });
            i += 2;
        } else {
            out.push(Tok { text: c.to_string(), line: n });
            i += 1;
        }
    }
}

fn is_ident(t: &str) -> bool {
    t.chars().next().is_some_and(is_ident_start)
}

/// `template <...>` at the head's start is not part of what it defines.
fn skip_template(head: &[Tok]) -> &[Tok] {
    if head.first().is_some_and(|t| t.text == "template") && head.get(1).is_some_and(|t| t.text == "<") {
        let mut depth = 0;
        for (i, t) in head.iter().enumerate().skip(1) {
            match t.text.as_str() {
                "<" => depth += 1,
                ">" => {
                    depth -= 1;
                    if depth == 0 {
                        return &head[i + 1..];
                    }
                }
                _ => {}
            }
        }
    }
    head
}

/// The function a head defines: the identifier before its first top-level
/// `(` that is not an attribute, unless a control word or an operator.
fn function_name(head: &[Tok]) -> Option<&Tok> {
    if head.iter().any(|t| t.text == "=") && !head.iter().any(|t| t.text == "operator") {
        // An initializer (`int x[] = {`), unless `operator=`.
        let eq = head.iter().position(|t| t.text == "=")?;
        let paren = head.iter().position(|t| t.text == "(")?;
        if eq < paren {
            return None;
        }
    }
    let mut depth = 0i32;
    let mut square = 0i32;
    for (i, t) in head.iter().enumerate() {
        match t.text.as_str() {
            "[" => square += 1,
            "]" => square -= 1,
            "(" if square > 0 => depth += 1,
            "(" => {
                if depth == 0 {
                    let prev = i.checked_sub(1).map(|j| &head[j]);
                    // `@Override(`, `@DataClass.Generated(`: an annotation's arguments.
                    let annotation = {
                        let mut k = i - 1;
                        while k >= 2 && head[k - 1].text == "." && is_ident(&head[k - 2].text) {
                            k -= 2;
                        }
                        k >= 1 && head[k - 1].text == "@"
                    };
                    match prev {
                        Some(p) if is_ident(&p.text) && (ATTRIBUTES.contains(&p.text.as_str()) || annotation) => {}
                        Some(p) if is_ident(&p.text) => {
                            let tilde = i >= 2 && head[i - 2].text == "~";
                            let op = head[..i].iter().rev().take(3).any(|t| t.text == "operator");
                            if CONTROL.contains(&p.text.as_str()) || tilde || op || p.text == "operator" {
                                return None;
                            }
                            return Some(p);
                        }
                        _ => return None,
                    }
                }
                depth += 1;
            }
            ")" => depth -= 1,
            _ => {}
        }
    }
    None
}

/// `struct X {`, `class EXPORT X : public Y {`: the name before `:` or the
/// end, outside parentheses; `None` for an anonymous type.
fn type_name(head: &[Tok]) -> Option<&Tok> {
    let kw = head.iter().position(|t| TYPE_KW.contains(&t.text.as_str()))?;
    let mut depth = 0i32;
    let mut name = None;
    for t in &head[kw + 1..] {
        match t.text.as_str() {
            "(" | "<" => depth += 1,
            ")" | ">" => depth -= 1,
            ":" | "extends" | "implements" | "where" if depth == 0 => break,
            s if depth == 0 && is_ident(s) && !NOT_NAMES.contains(&s) => name = Some(t),
            _ => {}
        }
    }
    name
}

/// The names a `typedef` without a body declares: `typedef int (*cb)(int);`
/// is `cb`, `typedef char buf[10];` is `buf`, else the last identifier.
fn typedef_names(head: &[Tok]) -> Vec<&Tok> {
    for w in head.windows(3) {
        if w[0].text == "(" && w[1].text == "*" && is_ident(&w[2].text) {
            return vec![&w[2]];
        }
    }
    let mut depth = 0i32;
    let mut last = None;
    for t in head {
        match t.text.as_str() {
            "(" | "[" => depth += 1,
            ")" | "]" => depth -= 1,
            s if depth == 0 && is_ident(s) && !NOT_NAMES.contains(&s) => last = Some(t),
            _ => {}
        }
    }
    last.into_iter().collect()
}

fn def(path: &str, raw: &[&str], name: &Tok, kind: &'static str, end: usize) -> Def {
    Def {
        path: path.to_string(),
        name: name.text.clone(),
        kind,
        line: name.line as u32 + 1,
        end: end as u32 + 1,
        anchor: raw.get(name.line).map(|l| l.trim().chars().take(160).collect()).unwrap_or_default(),
    }
}

pub fn definitions(path: &str, text: &str) -> Vec<Def> {
    let raw: Vec<&str> = text.lines().collect();
    let code = clean(text);
    let ext = crate::code::ext_of(path);
    // Java and C#: a method with no body (an interface's) is a definition,
    // and so is a C# property and an expression-bodied `=> x;` member.
    let jc = ext == "java" || ext == "cs";
    let mut out: Vec<Def> = Vec::new();
    // Open blocks: kind, and for a type the index of its Def in `out`, so
    // its end is set at the closing brace; `typedef` braces name the type
    // after the brace.
    let mut stack: Vec<(Block, Option<usize>, bool, usize)> = Vec::new();
    let mut head: Vec<Tok> = Vec::new();
    let mut paren = 0i32;
    // The line a typedef'd body opened on, until its names after the brace.
    let mut typedef_tail: Option<usize> = None;
    let mut enum_expect = false;
    let mut enum_count = 0usize;
    // Preprocessor: only the first branch of a conditional is read.
    let mut pp: Vec<bool> = Vec::new();
    let mut continued = false;
    let mut toks: Vec<Tok> = Vec::new();
    for (n, line) in code.iter().enumerate() {
        let t = line.trim_start();
        if continued || t.starts_with('#') {
            let was = continued;
            continued = line.trim_end().ends_with('\\');
            if was {
                continue;
            }
            let d = t[1..].trim_start();
            let word: String = d.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            match word.as_str() {
                "if" | "ifdef" | "ifndef" => {
                    let zero = word == "if" && d[2..].trim() == "0";
                    pp.push(!zero);
                }
                "else" | "elif" | "elifdef" | "elifndef" => {
                    if let Some(a) = pp.last_mut() {
                        *a = false;
                    }
                }
                "endif" => {
                    pp.pop();
                }
                "define" if pp.iter().all(|a| *a) => {
                    let rest = d[6..].trim_start();
                    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                    if !name.is_empty() && name.chars().next().is_some_and(is_ident_start) {
                        let func = rest[name.len()..].starts_with('(');
                        let mut end = n;
                        while code.get(end).is_some_and(|l| l.trim_end().ends_with('\\')) {
                            end += 1;
                        }
                        let tok = Tok { text: name, line: n };
                        out.push(def(path, &raw, &tok, if func { "fn" } else { "const" }, end));
                    }
                }
                _ => {}
            }
            continue;
        }
        if !pp.iter().all(|a| *a) {
            continue;
        }
        toks.clear();
        tokens(line, n, &mut toks);
        for tok in toks.drain(..) {
            let top = stack.last().map(|b| b.0).unwrap_or(Block::Container);
            match (top, tok.text.as_str()) {
                (Block::Body, "{") => stack.push((Block::Body, None, false, n)),
                (Block::Body, "}") => {
                    // A function's end is where its body closes.
                    if let Some((_, Some(i), _, _)) = stack.pop() {
                        out[i].end = n as u32 + 1;
                    }
                }
                (Block::Body, _) => {}
                (Block::Enum, "{") => stack.push((Block::Body, None, false, n)),
                (Block::Enum, "}") => {
                    close(&mut stack, &mut out, n, &mut typedef_tail, &mut head);
                    paren = 0;
                }
                (Block::Enum, ",") => enum_expect = true,
                // A Java enum's constants end at `;`; methods follow.
                (Block::Enum, ";") if jc => {
                    if let Some(top) = stack.last_mut() {
                        top.0 = Block::Container;
                    }
                }
                (Block::Enum, s) => {
                    if enum_expect && is_ident(s) && enum_count < MAX_VARIANTS_PER_ENUM {
                        out.push(def(path, &raw, &tok, "variant", n));
                        enum_count += 1;
                    }
                    enum_expect = false;
                }
                (Block::Container, "(") => {
                    paren += 1;
                    head.push(tok);
                }
                (Block::Container, ")") => {
                    paren -= 1;
                    head.push(tok);
                }
                (Block::Container, ";") if paren <= 0 => {
                    let in_type = stack.last().is_some_and(|b| b.1.is_some());
                    let arrow = head.windows(2).any(|w| w[0].text == "=" && w[1].text == ">");
                    if jc && in_type && (arrow || !head.iter().any(|t| t.text == "=")) {
                        if let Some(name) = function_name(&head) {
                            out.push(def(path, &raw, name, "fn", n));
                        }
                    }
                    if let Some(start) = typedef_tail.take() {
                        // `} Name, *PName;` after a typedef'd body: the
                        // span is the whole declaration.
                        // The last identifier of each comma group: `} FAR ct_data;` names `ct_data`.
                        let names: Vec<&Tok> = head.split(|t| t.text == ",").filter_map(|g| g.iter().rev().find(|t| is_ident(&t.text) && !NOT_NAMES.contains(&t.text.as_str()))).collect();
                        for t in names {
                            let mut d = def(path, &raw, t, "type", n);
                            d.line = start as u32 + 1;
                            d.anchor = raw.get(start).map(|l| l.trim().chars().take(160).collect()).unwrap_or_default();
                            out.push(d);
                        }
                    } else if head.first().is_some_and(|t| t.text == "typedef") {
                        for t in typedef_names(&head[1..]) {
                            out.push(def(path, &raw, t, "type", n));
                        }
                    }
                    head.clear();
                    paren = 0;
                }
                (Block::Container, "{") if paren <= 0 => {
                    let h = skip_template(&head);
                    let typedef = h.first().is_some_and(|t| t.text == "typedef");
                    let h = if typedef { &h[1..] } else { h };
                    // `@Entity`, `@Table(name = "x")` and `[Serializable]` are not the
                    // definition's first word.
                    let mut skip = vec![false; h.len()];
                    let (mut sq, mut i) = (0i32, 0usize);
                    while i < h.len() {
                        match h[i].text.as_str() {
                            "[" => sq += 1,
                            "]" => sq -= 1,
                            "@" if i + 1 < h.len() && h[i + 1].text != "interface" => {
                                // `@jdk.internal.ValueBased`: the whole dotted name.
                                let mut k = i + 1;
                                skip[k] = true;
                                while h.get(k + 1).is_some_and(|t| t.text == ".") && h.get(k + 2).is_some_and(|t| is_ident(&t.text)) {
                                    skip[k + 1] = true;
                                    skip[k + 2] = true;
                                    k += 2;
                                }
                                if h.get(k + 1).is_some_and(|t| t.text == "(") {
                                    let mut d = 0;
                                    for (j, t) in h.iter().enumerate().skip(k + 1) {
                                        d += (t.text == "(") as i32 - (t.text == ")") as i32;
                                        skip[j] = true;
                                        if d == 0 {
                                            break;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        if sq > 0 {
                            skip[i] = true;
                        }
                        i += 1;
                    }
                    let first_kw = h.iter().enumerate().find(|(i, t)| !skip[*i] && is_ident(&t.text) && !MODS.contains(&t.text.as_str())).map(|(_, t)| t);
                    let block = if h.iter().any(|t| t.text == "namespace") || (h.len() == 1 && h[0].text == "extern") {
                        (Block::Container, None, false, n)
                    } else if first_kw.is_some_and(|t| TYPE_KW.contains(&t.text.as_str())) && !h.iter().enumerate().any(|(i, t)| !skip[i] && t.text == "=") {
                        let is_enum = first_kw.is_some_and(|t| t.text == "enum");
                        let idx = type_name(h).map(|t| {
                            out.push(def(path, &raw, t, "type", n));
                            out.len() - 1
                        });
                        if is_enum {
                            enum_expect = true;
                            enum_count = 0;
                            (Block::Enum, idx, typedef, n)
                        } else {
                            (Block::Container, idx, typedef, n)
                        }
                    } else if let Some(name) = function_name(h) {
                        out.push(def(path, &raw, name, "fn", n));
                        (Block::Body, Some(out.len() - 1), false, n)
                    } else {
                        // `public string Name { get; set; }`: a C# property.
                        let in_type = stack.last().is_some_and(|b| b.1.is_some());
                        if ext == "cs" && in_type && !h.iter().any(|t| t.text == "=") {
                            if let Some(t) = h.last().filter(|t| is_ident(&t.text) && !MODS.contains(&t.text.as_str()) && !NOT_NAMES.contains(&t.text.as_str())) {
                                out.push(def(path, &raw, t, "val", n));
                            }
                        }
                        (Block::Body, None, false, n)
                    };
                    stack.push(block);
                    head.clear();
                }
                (Block::Container, "}") if paren <= 0 => {
                    close(&mut stack, &mut out, n, &mut typedef_tail, &mut head);
                }
                (Block::Container, _) => head.push(tok),
            }
        }
    }
    out
}

/// Closes the innermost block: a type's end is its closing brace, and a
/// typedef'd body names its type after the brace.
fn close(stack: &mut Vec<(Block, Option<usize>, bool, usize)>, out: &mut [Def], n: usize, typedef_tail: &mut Option<usize>, head: &mut Vec<Tok>) {
    head.clear();
    if let Some((_, idx, typedef, start)) = stack.pop() {
        if let Some(i) = idx {
            out[i].end = n as u32 + 1;
        }
        if typedef {
            *typedef_tail = Some(start);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(src: &str) -> Vec<(String, &'static str, u32, u32)> {
        definitions("a.c", src).into_iter().map(|d| (d.name, d.kind, d.line, d.end)).collect()
    }

    #[test]
    fn c_functions_macros_and_types() {
        let src = "#include \"a.h\"\n#define MAX 10\n#define SQ(x) ((x)*(x))\n\nstatic int\nfoo (int x)\n{\n  int local = 1;\n  if (x) { return 1; }\n  return 0;\n}\n\ntypedef struct node {\n  int v;\n} Node, *PNode;\n\ntypedef int (*cb)(int);\ntypedef unsigned long ulong;\nenum colour { RED, GREEN = 2 };\nint bar(void) { return foo(1); }\nint proto(int);\nstatic const int table[] = { 1, 2 };\n";
        let d = names(src);
        let has = |n: &str, k: &str| d.iter().any(|x| x.0 == n && x.1 == k);
        for (n, k) in [("MAX", "const"), ("SQ", "fn"), ("foo", "fn"), ("node", "type"), ("Node", "type"), ("PNode", "type"), ("cb", "type"), ("ulong", "type"), ("colour", "type"), ("RED", "variant"), ("GREEN", "variant"), ("bar", "fn")] {
            assert!(has(n, k), "{n} {k} in {d:?}");
        }
        for bad in ["int", "local", "proto", "table", "x", "v", "static"] {
            assert!(!d.iter().any(|x| x.0 == bad), "{bad} is not a definition: {d:?}");
        }
        let foo = d.iter().find(|x| x.0 == "foo").unwrap();
        assert_eq!((foo.2, foo.3), (6, 11), "GNU style: name line to closing brace");
    }

    #[test]
    fn cpp_methods_namespaces_and_conditionals() {
        let src = "namespace gr {\nclass EXPORT Face : public Base {\npublic:\n  int size() const { return n; }\n  void load(int);\n};\nFace::Face(int a) : n(a) {\n}\nvoid Face::load(int a) {\n  for (int i = 0; i < a; ++i) {}\n}\n}\nextern \"C\" {\n#if X\nint twice(int a) {\n#else\nint twice(int a, int b) {\n#endif\n  return a;\n}\n}\nbool operator==(const A& a, const B& b) { return true; }\n";
        let d = names(src);
        let has = |n: &str, k: &str| d.iter().any(|x| x.0 == n && x.1 == k);
        for (n, k) in [("Face", "type"), ("size", "fn"), ("Face", "fn"), ("load", "fn"), ("twice", "fn")] {
            assert!(has(n, k), "{n} {k} in {d:?}");
        }
        assert_eq!(d.iter().filter(|x| x.0 == "twice").count(), 1, "one branch of an #if: {d:?}");
        assert!(!d.iter().any(|x| x.0 == "for" || x.0 == "operator" || x.0 == "EXPORT" || x.0 == "i"), "{d:?}");
    }
    #[test]
    fn java_and_csharp_methods_have_no_keyword() {
        let java = "@jdk.internal.ValueBased\npublic final class Integer extends Number implements Comparable<Integer> {\n    @Native public static final int MIN = 0;\n    public Integer(int v) { }\n    static String name(String a) {\n        return a;\n    }\n    @Override\n    public int hashCode() { return 0; }\n    interface Cb { void run(); }\n    enum Mode { A, B; int code() { return 0; } }\n}\n@interface Generated { }\n";
        let d: Vec<(String, &str)> = definitions("A.java", java).into_iter().map(|d| (d.name, d.kind)).collect();
        let names: Vec<&str> = d.iter().filter(|x| x.1 != "variant" && x.1 != "const").map(|x| x.0.as_str()).collect();
        assert_eq!(names, ["Integer", "Integer", "name", "hashCode", "Cb", "run", "Mode", "code", "Generated"], "{d:?}");
        let cs = "namespace X {\n    [Serializable]\n    public class Store {\n        public int Size() { return 1; }\n        private static async Task<int> LoadAsync(string p) { return 0; }\n        public string Name { get; set; }\n    }\n}\n";
        let names: Vec<String> = definitions("A.cs", cs).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Store", "Size", "LoadAsync", "Name"]);
    }
}
