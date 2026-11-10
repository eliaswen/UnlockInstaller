//! Minimal Valve KeyValues (VDF/ACF) text parser.

#[derive(Debug, Clone)]
pub enum Vdf {
    Str(String),
    Obj(Vec<(String, Vdf)>),
}

impl Vdf {
    pub fn get(&self, key: &str) -> Option<&Vdf> {
        match self {
            Vdf::Obj(items) => items
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Vdf::Str(_) => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Vdf::Str(s) => Some(s),
            Vdf::Obj(_) => None,
        }
    }
    pub fn entries(&self) -> &[(String, Vdf)] {
        match self {
            Vdf::Obj(items) => items,
            Vdf::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Tok {
    Str(String),
    Open,
    Close,
}

fn tokenize(src: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '{' => {
                chars.next();
                toks.push(Tok::Open);
            }
            '}' => {
                chars.next();
                toks.push(Tok::Close);
            }
            '/' => {
                // `//` line comment
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(other) => s.push(other),
                            None => break,
                        },
                        c => s.push(c),
                    }
                }
                toks.push(Tok::Str(s));
            }
            _ => {
                let mut s = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    chars.next();
                }
                toks.push(Tok::Str(s));
            }
        }
    }
    toks
}

fn parse_obj(toks: &mut std::iter::Peekable<std::vec::IntoIter<Tok>>) -> Vdf {
    let mut items = Vec::new();
    loop {
        match toks.next() {
            Some(Tok::Str(key)) => match toks.next() {
                Some(Tok::Str(v)) => items.push((key, Vdf::Str(v))),
                Some(Tok::Open) => items.push((key, parse_obj(toks))),
                _ => break,
            },
            Some(Tok::Close) | None => break,
            Some(Tok::Open) => {
                // stray brace, skip nested block
                parse_obj(toks);
            }
        }
    }
    Vdf::Obj(items)
}

pub fn parse_vdf(src: &str) -> Vdf {
    let mut toks = tokenize(src).into_iter().peekable();
    parse_obj(&mut toks)
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_libraryfolders() {
        let src = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"/home/u/.local/share/Steam"
		"apps" { "228980" "123" }
	}
	"1" { "path" "/mnt/games/Steam\\Lib" }
}"#;
        let v = parse_vdf(src);
        let f = v.get("libraryfolders").unwrap();
        let paths: Vec<_> = f
            .entries()
            .iter()
            .filter_map(|(_, e)| e.get("path").and_then(Vdf::str))
            .collect();
        assert_eq!(paths, ["/home/u/.local/share/Steam", "/mnt/games/Steam\\Lib"]);
    }

    #[test]
    fn parses_acf() {
        let src = "\"AppState\"\n{\n\t\"appid\"\t\t\"620\"\n\t\"name\"\t\t\"Portal 2\"\n\t\"installdir\"\t\t\"Portal 2\"\n}";
        let v = parse_vdf(src);
        let s = v.get("appstate").unwrap();
        assert_eq!(s.get("appid").and_then(Vdf::str), Some("620"));
        assert_eq!(s.get("name").and_then(Vdf::str), Some("Portal 2"));
    }
}
