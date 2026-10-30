//! Importers for what other browsers export: Google Password Manager / Chrome / Firefox password
//! CSVs, and the standard bookmarks HTML file every browser (and Google Takeout) produces.

/// `(url, username, password)` rows from a passwords CSV, matching columns by their header.
pub fn import_passwords_csv(text: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut rows = parse_csv(text).into_iter();
    let header: Vec<String> = rows.next().ok_or("the file is empty")?.iter().map(|h| h.trim().to_ascii_lowercase()).collect();
    let col = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let url = col(&["url", "origin", "website", "login_uri"]).ok_or("no url column — is this a passwords export?")?;
    let user = col(&["username", "login", "login_username", "email"]).ok_or("no username column")?;
    let pass = col(&["password", "login_password"]).ok_or("no password column")?;
    Ok(rows
        .filter_map(|r| Some((r.get(url)?.trim().to_owned(), r.get(user)?.trim().to_owned(), r.get(pass)?.to_owned())))
        .filter(|(u, _, p)| !u.is_empty() && !p.is_empty())
        .collect())
}

/// RFC 4180: quoted fields, doubled quotes, newlines inside quotes.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if field.is_empty() => quoted = true,
            (',', false) => row.push(std::mem::take(&mut field)),
            ('\r', false) => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            (c, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// `(title, url)` pairs from a Netscape bookmarks file (`<A HREF="…">title</A>`).
pub fn import_bookmarks_html(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(start) = lower[at..].find("<a ").map(|i| at + i) {
        let Some(tag_end) = lower[start..].find('>').map(|i| start + i) else { break };
        let Some(close) = lower[tag_end..].find("</a>").map(|i| tag_end + i) else { break };
        let tag = &html[start..tag_end];
        if let Some(href) = attribute(tag, "href") {
            let title = decode_entities(html[tag_end + 1..close].trim());
            out.push((if title.is_empty() { href.clone() } else { title }, href));
        }
        at = close + 4;
    }
    out
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(&format!("{name}=\""))? + name.len() + 2;
    let end = tag[i..].find('"')? + i;
    Some(decode_entities(&tag[i..end]))
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_password_manager_csv() {
        let csv = "name,url,username,password,note\r\n\
                   github.com,https://github.com/login,me@x.org,\"p,a\"\"ss\",\r\n\
                   bad,,u,p,\r\n";
        let rows = import_passwords_csv(csv).unwrap();
        assert_eq!(rows, vec![("https://github.com/login".into(), "me@x.org".into(), "p,a\"ss".into())]);
        assert!(import_passwords_csv("a,b\n1,2\n").is_err());
    }

    #[test]
    fn firefox_csv_columns() {
        let csv = "\"url\",\"username\",\"password\",\"httpRealm\"\n\"https://a.example\",\"u\",\"p\",\"\"\n";
        assert_eq!(import_passwords_csv(csv).unwrap().len(), 1);
    }

    #[test]
    fn bookmarks_html() {
        let html = r#"<DL><p><DT><A HREF="https://example.com/?a=1&amp;b=2" ADD_DATE="1">Ex &amp; co</A>
                      <DT><H3>Folder</H3><DL><DT><A HREF="https://rust-lang.org">Rust</A></DL>"#;
        assert_eq!(
            import_bookmarks_html(html),
            vec![("Ex & co".into(), "https://example.com/?a=1&b=2".into()), ("Rust".into(), "https://rust-lang.org".into())]
        );
    }
}
