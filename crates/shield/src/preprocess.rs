//! uBlock Origin's `!#if` / `!#else` / `!#endif` list preprocessor. adblock-rust doesn't
//! evaluate these, so lists are filtered before parsing.

/// Capability tokens that are true for us: a Chromium browser using uBO-flavoured lists.
const TRUE_TOKENS: &[&str] = &["ext_ublock", "env_chromium", "cap_user_stylesheet", "adguard_ext_chromium"];

pub fn preprocess(list: &str) -> String {
    let mut out = String::with_capacity(list.len());
    // Each entry: is this block active, and was its enclosing scope active.
    let mut stack: Vec<(bool, bool)> = Vec::new();
    let active = |stack: &[(bool, bool)]| stack.last().is_none_or(|&(on, outer)| on && outer);

    for line in list.lines() {
        if let Some(cond) = line.strip_prefix("!#if ") {
            let outer = active(&stack);
            stack.push((eval(cond), outer));
        } else if line.starts_with("!#else") {
            if let Some(top) = stack.last_mut() {
                top.0 = !top.0;
            }
        } else if line.starts_with("!#endif") {
            stack.pop();
        } else if active(&stack) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Evaluates `!`, `&&`, `||` and parentheses over tokens. Unknown tokens are false.
fn eval(expr: &str) -> bool {
    let tokens = tokenize(expr);
    let mut pos = 0;
    let value = parse_or(&tokens, &mut pos);
    pos == tokens.len() && value
}

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Ident(&'a str),
    Not,
    And,
    Or,
    Open,
    Close,
}

fn tokenize(expr: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b' ' | b'\t' => i += 1,
            b'!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            b'(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            b'&' if bytes.get(i + 1) == Some(&b'&') => {
                tokens.push(Token::And);
                i += 2;
            }
            b'|' if bytes.get(i + 1) == Some(&b'|') => {
                tokens.push(Token::Or);
                i += 2;
            }
            _ => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if i == start {
                    // Unknown character: make the whole expression false.
                    return vec![Token::Close];
                }
                tokens.push(Token::Ident(&expr[start..i]));
            }
        }
    }
    tokens
}

fn parse_or(t: &[Token], pos: &mut usize) -> bool {
    let mut value = parse_and(t, pos);
    while t.get(*pos) == Some(&Token::Or) {
        *pos += 1;
        value |= parse_and(t, pos);
    }
    value
}

fn parse_and(t: &[Token], pos: &mut usize) -> bool {
    let mut value = parse_unary(t, pos);
    while t.get(*pos) == Some(&Token::And) {
        *pos += 1;
        value &= parse_unary(t, pos);
    }
    value
}

fn parse_unary(t: &[Token], pos: &mut usize) -> bool {
    match t.get(*pos) {
        Some(Token::Not) => {
            *pos += 1;
            !parse_unary(t, pos)
        }
        Some(Token::Open) => {
            *pos += 1;
            let value = parse_or(t, pos);
            if t.get(*pos) == Some(&Token::Close) {
                *pos += 1;
            }
            value
        }
        Some(Token::Ident(name)) => {
            *pos += 1;
            TRUE_TOKENS.contains(name)
        }
        _ => {
            *pos = t.len() + 1;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_expressions() {
        assert!(eval("env_chromium"));
        assert!(!eval("env_firefox"));
        assert!(eval("!env_mobile"));
        assert!(eval("env_chromium && !env_mv3"));
        assert!(!eval("env_safari || cap_html_filtering"));
        assert!(eval("(env_firefox || env_chromium) && ext_ublock"));
        assert!(!eval("env_chromium &&"));
    }

    #[test]
    fn filters_blocks() {
        let list = "a\n!#if env_firefox\nb\n!#else\nc\n!#endif\n!#if env_chromium\nd\n!#if env_mobile\ne\n!#endif\nf\n!#endif\ng\n";
        assert_eq!(preprocess(list), "a\nc\nd\nf\ng\n");
    }

    #[test]
    fn else_inside_inactive_parent_stays_off() {
        let list = "!#if env_firefox\n!#if env_mobile\nx\n!#else\ny\n!#endif\n!#endif\n";
        assert_eq!(preprocess(list), "");
    }
}
