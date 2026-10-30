//! Dev tool: turns uBlock Origin's scriptlets and redirect resources into adblock-rust's
//! resource JSON. uBO's JavaScript is only *read as text* here, never executed.
//!
//! cargo run -p unskilled-shield --features resource-assembler --bin ubo-resources -- \
//!     <uBlock checkout> <brave resources.json> > crates/shield/assets/resources.json

use adblock::resources::{MimeType, PermissionMask, Resource, ResourceType};
use base64::Engine as _;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

/// uBO's "trusted" scriptlets are only injectable by lists parsed with this permission.
const TRUSTED: PermissionMask = PermissionMask::from_bits(1);

fn main() {
    let mut args = std::env::args().skip(1);
    let ubo = args.next().expect("usage: ubo-resources <uBlock dir> [brave resources.json]");
    let brave = args.next();
    let ubo = Path::new(&ubo);

    let mut resources = scriptlets(&ubo.join("src/js/resources"));
    eprintln!("scriptlets: {}", resources.len());

    #[allow(deprecated)]
    let redirects = adblock::resources::resource_assembler::assemble_web_accessible_resources(
        &ubo.join("src/web_accessible_resources"),
        &ubo.join("src/js/redirect-resources.js"),
    );
    eprintln!("redirect resources: {}", redirects.len());
    resources.extend(redirects);

    if let Some(brave) = brave {
        let extra: Vec<Resource> = serde_json::from_str(&fs::read_to_string(brave).expect("read brave resources"))
            .expect("parse brave resources");
        eprintln!("brave resources: {}", extra.len());
        resources.extend(extra);
    }

    println!("{}", serde_json::to_string(&resources).unwrap());
}

struct Registration {
    name: String,
    aliases: Vec<String>,
    function: String,
    dependencies: Vec<Dep>,
    trusted: bool,
}

enum Dep {
    Name(String),
    Function(String),
}

fn scriptlets(dir: &Path) -> Vec<Resource> {
    let mut functions = HashMap::new();
    let mut registrations = Vec::new();
    let mut queue: Vec<PathBuf> = fs::read_dir(dir).expect("read resources dir").flatten().map(|e| e.path()).collect();
    queue.retain(|p| p.extension().is_some_and(|e| e == "js"));
    queue.sort();
    queue.reverse();
    // Scriptlets may import helpers from modules outside the resources dir (e.g. `../urlskip.js`).
    let mut seen = HashSet::new();
    while let Some(path) = queue.pop() {
        let path = path.canonicalize().expect("resolve module path");
        if !seen.insert(path.clone()) {
            continue;
        }
        let src = fs::read_to_string(&path).expect("read scriptlet module");
        scan_module(&src, &mut functions, &mut registrations);
        let base = path.parent().unwrap();
        queue.extend(relative_imports(&src).map(|i| base.join(i)));
    }

    let name_of_fn: HashMap<&str, &str> =
        registrations.iter().map(|r| (r.function.as_str(), r.name.as_str())).collect();

    registrations
        .iter()
        .map(|r| {
            let source = functions
                .get(&r.function)
                .unwrap_or_else(|| panic!("no source for function {} ({})", r.function, r.name));
            let dependencies = r
                .dependencies
                .iter()
                .map(|d| match d {
                    Dep::Name(n) => n.clone(),
                    Dep::Function(f) => {
                        name_of_fn.get(f.as_str()).unwrap_or_else(|| panic!("unregistered dependency {f}")).to_string()
                    }
                })
                .collect();
            Resource {
                name: r.name.clone(),
                aliases: r.aliases.clone(),
                kind: ResourceType::Mime(MimeType::ApplicationJavascript),
                content: base64::engine::general_purpose::STANDARD.encode(source),
                dependencies,
                permission: if r.trusted { TRUSTED } else { PermissionMask::default() },
            }
        })
        .collect()
}

/// `import ... from './x.js'` targets of a module.
fn relative_imports(src: &str) -> impl Iterator<Item = &str> {
    src.lines().filter(|l| l.starts_with("import ") || l.starts_with("} from")).filter_map(|l| {
        let quote = l.find(['\'', '"'])?;
        let rest = &l[quote + 1..];
        let path = &rest[..rest.find(['\'', '"'])?];
        path.starts_with('.').then_some(path)
    })
}

/// Collects top-level `function name(...) {...}` sources and scriptlet registrations.
fn scan_module(src: &str, functions: &mut HashMap<String, String>, registrations: &mut Vec<Registration>) {
    let b = src.as_bytes();
    let mut i = 0;
    let mut prev = b';';
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if let Some(end) = skip_non_code(b, i, prev) {
            i = end;
            continue;
        }
        if is_ident_start(c) && (i == 0 || !is_ident(b[i - 1])) {
            let word_end = ident_end(b, i);
            let word = &src[i..word_end];
            match word {
                "function" => {
                    let name_start = skip_ws(b, word_end);
                    let name_end = ident_end(b, name_start);
                    let name = &src[name_start..name_end];
                    let paren = skip_ws(b, name_end);
                    if !name.is_empty() && b.get(paren) == Some(&b'(') {
                        let params_end = match_bracket(b, paren);
                        let body = skip_ws(b, params_end);
                        assert_eq!(b[body], b'{', "function {name}: expected body");
                        let end = match_bracket(b, body);
                        let start = if src[..i].trim_end().ends_with("async") {
                            src[..i].trim_end().len() - "async".len()
                        } else {
                            i
                        };
                        functions.insert(name.to_owned(), src[start..end].to_owned());
                        i = end;
                        prev = b'}';
                        continue;
                    }
                }
                // Helper classes (ArglistParser, JSONPath) are injected as dependencies verbatim.
                "class" => {
                    let name_start = skip_ws(b, word_end);
                    let name_end = ident_end(b, name_start);
                    let body = skip_ws(b, name_end);
                    if name_end > name_start && b.get(body) == Some(&b'{') {
                        let end = match_bracket(b, body);
                        functions.insert(src[name_start..name_end].to_owned(), src[i..end].to_owned());
                        i = end;
                        prev = b'}';
                        continue;
                    }
                }
                "registerScriptlet" => {
                    let paren = skip_ws(b, word_end);
                    if b.get(paren) == Some(&b'(') {
                        let end = match_bracket(b, paren);
                        let mut p = Parser { src, pos: paren + 1 };
                        let function = p.ident();
                        p.expect(b',');
                        let reg = p.object(Some(function));
                        registrations.push(reg);
                        i = end;
                        prev = b')';
                        continue;
                    }
                }
                "builtinScriptlets" if src[word_end..].starts_with(".push(") => {
                    let paren = word_end + ".push".len();
                    let end = match_bracket(b, paren);
                    let mut p = Parser { src, pos: paren + 1 };
                    registrations.push(p.object(None));
                    i = end;
                    prev = b')';
                    continue;
                }
                _ => {}
            }
            i = word_end;
            prev = b'a';
            continue;
        }
        if c == b'{' || c == b'(' || c == b'[' {
            // Only top-level declarations matter; skip nested code whole.
            i = match_bracket(b, i);
            prev = b'}';
            continue;
        }
        prev = c;
        i += 1;
    }
}

/// Minimal parser for uBO's registration object literals.
struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        let b = self.src.as_bytes();
        loop {
            self.pos = skip_ws(b, self.pos);
            if self.src[self.pos..].starts_with("//") || self.src[self.pos..].starts_with("/*") {
                self.pos = skip_non_code(b, self.pos, b',').unwrap();
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) {
        self.ws();
        assert_eq!(self.src.as_bytes()[self.pos] as char, c as char, "at {}", &self.src[self.pos..self.pos + 40]);
        self.pos += 1;
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.src.as_bytes().get(self.pos) == Some(&c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> String {
        self.ws();
        let end = ident_end(self.src.as_bytes(), self.pos);
        let s = self.src[self.pos..end].to_owned();
        self.pos = end;
        s
    }

    fn string(&mut self) -> String {
        self.ws();
        let b = self.src.as_bytes();
        let quote = b[self.pos];
        assert!(quote == b'\'' || quote == b'"', "expected string at {}", &self.src[self.pos..self.pos + 40]);
        let end = skip_string(b, self.pos);
        let s = self.src[self.pos + 1..end - 1].to_owned();
        self.pos = end;
        s
    }

    fn object(&mut self, function: Option<String>) -> Registration {
        let mut reg = Registration {
            name: String::new(),
            aliases: vec![],
            function: function.unwrap_or_default(),
            dependencies: vec![],
            trusted: false,
        };
        self.expect(b'{');
        while !self.eat(b'}') {
            let key = self.ident();
            self.expect(b':');
            match key.as_str() {
                "name" => reg.name = self.string(),
                "fn" => reg.function = self.ident(),
                "aliases" => reg.aliases = self.array().into_iter().map(|d| match d {
                    Dep::Name(n) => n,
                    Dep::Function(f) => f,
                }).collect(),
                "dependencies" => reg.dependencies = self.array(),
                "requiresTrust" => reg.trusted = self.ident() == "true",
                _ => {
                    // world, etc.: a string or identifier we don't need.
                    self.ws();
                    if matches!(self.src.as_bytes()[self.pos], b'\'' | b'"') {
                        self.string();
                    } else {
                        self.ident();
                    }
                }
            }
            self.eat(b',');
        }
        assert!(!reg.name.is_empty() && !reg.function.is_empty(), "incomplete registration");
        reg
    }

    fn array(&mut self) -> Vec<Dep> {
        let mut items = vec![];
        self.expect(b'[');
        while !self.eat(b']') {
            self.ws();
            if matches!(self.src.as_bytes()[self.pos], b'\'' | b'"') {
                items.push(Dep::Name(self.string()));
            } else {
                items.push(Dep::Function(self.ident()));
            }
            self.eat(b',');
        }
        items
    }
}

// ── JavaScript-aware scanning ────────────────────────────────────────────────

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn is_ident(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

fn ident_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && is_ident(b[i]) {
        i += 1;
    }
    i
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// If a string, template, comment or regex literal starts at `i`, returns the index after it.
/// `prev` is the previous significant byte, used to tell a regex from a division.
fn skip_non_code(b: &[u8], i: usize, prev: u8) -> Option<usize> {
    match b[i] {
        b'\'' | b'"' => Some(skip_string(b, i)),
        b'`' => Some(skip_template(b, i)),
        b'/' if b.get(i + 1) == Some(&b'/') => {
            Some(b[i..].iter().position(|&c| c == b'\n').map_or(b.len(), |p| i + p))
        }
        b'/' if b.get(i + 1) == Some(&b'*') => {
            let rest = &b[i + 2..];
            Some(rest.windows(2).position(|w| w == b"*/").map_or(b.len(), |p| i + 2 + p + 2))
        }
        b'/' if regex_allowed(b, i, prev) => Some(skip_regex(b, i)),
        _ => None,
    }
}

fn regex_allowed(b: &[u8], i: usize, prev: u8) -> bool {
    if is_ident(prev) {
        // After a keyword a regex can follow; after an identifier/number it's a division.
        let mut end = i;
        while end > 0 && b[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        let mut start = end;
        while start > 0 && is_ident(b[start - 1]) {
            start -= 1;
        }
        let word = std::str::from_utf8(&b[start..end]).unwrap_or("");
        return matches!(
            word,
            "return" | "typeof" | "case" | "in" | "of" | "delete" | "void" | "throw" | "new" | "else" | "do" | "yield" | "await"
        );
    }
    !matches!(prev, b')' | b']')
}

fn skip_string(b: &[u8], i: usize) -> usize {
    let quote = b[i];
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

fn skip_template(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'`' => return j + 1,
            b'$' if b.get(j + 1) == Some(&b'{') => j = match_bracket(b, j + 1),
            _ => j += 1,
        }
    }
    b.len()
}

fn skip_regex(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    let mut in_class = false;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'[' => {
                in_class = true;
                j += 1;
            }
            b']' => {
                in_class = false;
                j += 1;
            }
            b'/' if !in_class => {
                return ident_end(b, j + 1);
            }
            b'\n' => return j,
            _ => j += 1,
        }
    }
    b.len()
}

/// `b[open]` is `(`, `[` or `{`; returns the index after its matching closer.
fn match_bracket(b: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut i = open;
    let mut prev = b'(';
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if let Some(end) = skip_non_code(b, i, prev) {
            // Comments don't change what may follow; literals act like operands.
            if c == b'/' && matches!(b.get(i + 1), Some(b'/') | Some(b'*')) {
                i = end;
            } else {
                i = end;
                prev = b'a';
                if b[end - 1] == b'/' || is_ident(b[end - 1]) {
                    prev = b')';
                }
            }
            continue;
        }
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        if is_ident(c) {
            let end = ident_end(b, i);
            i = end;
            prev = b[end - 1];
            continue;
        }
        prev = c;
        i += 1;
    }
    panic!("unbalanced bracket at byte {open}");
}
