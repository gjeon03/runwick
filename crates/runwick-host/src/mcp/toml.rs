//! A small TOML reader for provider configuration files (Codex `config.toml`). It understands
//! tables, arrays of tables, dotted keys, strings (basic, literal, multi-line), arrays, inline
//! tables, booleans and numbers; dates and other scalars are kept as text. The result is JSON so
//! MCP definitions from Claude (JSON) and Codex (TOML) are handled the same way.

use serde_json::{Map, Value};

pub fn parse(src: &str) -> Result<Value, String> {
    let mut p = Parser { c: src.chars().collect(), i: 0 };
    let mut root = Value::Object(Map::new());
    let mut table: Vec<Seg> = Vec::new();
    loop {
        p.skip_blank_lines();
        let Some(ch) = p.peek() else { break };
        if ch == '[' {
            let array = p.peek_at(1) == Some('[');
            p.i += if array { 2 } else { 1 };
            let path = p.key_path()?;
            p.expect(']')?;
            if array {
                p.expect(']')?;
            }
            p.end_of_line()?;
            table = path.into_iter().map(Seg::Key).collect();
            if array {
                let arr = node(&mut root, &table, true)?;
                let list = arr.as_array_mut().ok_or("배열이 아닌 키에 [[표]]가 있어요")?;
                list.push(Value::Object(Map::new()));
                table.push(Seg::Index(list.len() - 1));
            } else {
                node(&mut root, &table, false)?;
            }
            continue;
        }
        let key = p.key_path()?;
        p.skip_space();
        p.expect('=')?;
        p.skip_space();
        let value = p.value()?;
        p.end_of_line()?;
        let (last, parents) = key.split_last().ok_or("빈 키")?;
        let mut path = table.clone();
        path.extend(parents.iter().cloned().map(Seg::Key));
        let obj = node(&mut root, &path, false)?.as_object_mut().ok_or("표가 아닌 값에 키를 넣을 수 없어요")?;
        obj.insert(last.clone(), value);
    }
    Ok(root)
}

#[derive(Clone)]
enum Seg {
    Key(String),
    Index(usize),
}

/// The value at `path`, creating tables on the way (an array when `array` and missing).
fn node<'a>(root: &'a mut Value, path: &[Seg], array: bool) -> Result<&'a mut Value, String> {
    let mut cur = root;
    for (n, seg) in path.iter().enumerate() {
        let last = n + 1 == path.len();
        cur = match seg {
            Seg::Key(k) => {
                let obj = match cur {
                    Value::Array(a) => a.last_mut().and_then(|v| v.as_object_mut()),
                    Value::Object(o) => Some(o),
                    _ => None,
                }
                .ok_or_else(|| format!("{k}: 표 안에 있어야 해요"))?;
                obj.entry(k.clone())
                    .or_insert_with(|| if last && array { Value::Array(Vec::new()) } else { Value::Object(Map::new()) })
            }
            Seg::Index(i) => cur.as_array_mut().and_then(|a| a.get_mut(*i)).ok_or("표 배열 위치가 잘못됐어요")?,
        };
    }
    Ok(cur)
}

struct Parser {
    c: Vec<char>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.c.get(self.i + n).copied()
    }

    fn starts(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(n, ch)| self.peek_at(n) == Some(ch))
    }

    fn expect(&mut self, ch: char) -> Result<(), String> {
        self.skip_space();
        if self.peek() == Some(ch) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("{}번째 글자 근처에 '{ch}'가 필요해요", self.i))
        }
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while self.peek().is_some_and(|c| c != '\n') {
                self.i += 1;
            }
        }
    }

    fn skip_blank_lines(&mut self) {
        loop {
            self.skip_space();
            self.skip_comment();
            match self.peek() {
                Some('\n' | '\r') => self.i += 1,
                _ => break,
            }
        }
    }

    /// Whitespace, comments and newlines inside arrays and between values.
    fn skip_ws_all(&mut self) {
        self.skip_blank_lines();
    }

    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_space();
        self.skip_comment();
        match self.peek() {
            None | Some('\n') | Some('\r') => Ok(()),
            Some(c) => Err(format!("{}번째 글자 근처에 예상하지 못한 '{c}'가 있어요", self.i)),
        }
    }

    fn key_path(&mut self) -> Result<Vec<String>, String> {
        let mut parts = Vec::new();
        loop {
            self.skip_space();
            let part = match self.peek() {
                Some('"') => self.basic_string()?,
                Some('\'') => self.literal_string()?,
                _ => {
                    let start = self.i;
                    while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                        self.i += 1;
                    }
                    if start == self.i {
                        return Err(format!("{}번째 글자 근처에 키가 필요해요", self.i));
                    }
                    self.c[start..self.i].iter().collect()
                }
            };
            parts.push(part);
            self.skip_space();
            if self.peek() == Some('.') {
                self.i += 1;
            } else {
                return Ok(parts);
            }
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some('"') if self.starts("\"\"\"") => self.multiline_basic().map(Value::String),
            Some('\'') if self.starts("'''") => self.multiline_literal().map(Value::String),
            Some('"') => self.basic_string().map(Value::String),
            Some('\'') => self.literal_string().map(Value::String),
            Some('[') => self.array(),
            Some('{') => self.inline_table(),
            Some(_) => Ok(self.scalar()),
            None => Err("값이 없어요".into()),
        }
    }

    fn scalar(&mut self) -> Value {
        let start = self.i;
        while self.peek().is_some_and(|c| !matches!(c, ',' | ']' | '}' | '\n' | '\r' | '#' | ' ' | '\t')) {
            self.i += 1;
        }
        let raw: String = self.c[start..self.i].iter().collect::<String>().trim().to_string();
        match raw.as_str() {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => {
                let num = raw.replace('_', "");
                if let Ok(n) = num.parse::<i64>() {
                    Value::from(n)
                } else if let Some(f) = num.parse::<f64>().ok().and_then(serde_json::Number::from_f64) {
                    Value::Number(f)
                } else {
                    Value::String(raw)
                }
            }
        }
    }

    fn array(&mut self) -> Result<Value, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            self.skip_ws_all();
            if self.peek() == Some(']') {
                self.i += 1;
                return Ok(Value::Array(out));
            }
            out.push(self.value()?);
            self.skip_ws_all();
            match self.peek() {
                Some(',') => self.i += 1,
                Some(']') => {}
                _ => return Err(format!("{}번째 글자 근처의 배열이 닫히지 않았어요", self.i)),
            }
        }
    }

    fn inline_table(&mut self) -> Result<Value, String> {
        self.i += 1;
        let mut root = Value::Object(Map::new());
        loop {
            self.skip_space();
            if self.peek() == Some('}') {
                self.i += 1;
                return Ok(root);
            }
            let key = self.key_path()?;
            self.expect('=')?;
            self.skip_space();
            let v = self.value()?;
            let (last, parents) = key.split_last().ok_or("빈 키")?;
            let path: Vec<Seg> = parents.iter().cloned().map(Seg::Key).collect();
            node(&mut root, &path, false)?
                .as_object_mut()
                .ok_or("표가 아닌 값에 키를 넣을 수 없어요")?
                .insert(last.clone(), v);
            self.skip_space();
            match self.peek() {
                Some(',') => self.i += 1,
                Some('}') => {}
                _ => return Err(format!("{}번째 글자 근처의 인라인 표가 닫히지 않았어요", self.i)),
            }
        }
    }

    fn basic_string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some('\n') => return Err("문자열이 닫히지 않았어요".into()),
                Some('"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some('\\') => {
                    self.i += 1;
                    out.push(self.escape()?);
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        let c = self.peek().ok_or("이스케이프가 끝나지 않았어요")?;
        self.i += 1;
        Ok(match c {
            'b' => '\u{8}',
            't' => '\t',
            'n' => '\n',
            'f' => '\u{c}',
            'r' => '\r',
            'e' => '\u{1b}',
            '"' => '"',
            '\\' => '\\',
            'u' | 'U' => {
                let len = if c == 'u' { 4 } else { 8 };
                let hex: String =
                    self.c.get(self.i..self.i + len).ok_or("유니코드 이스케이프가 짧아요")?.iter().collect();
                self.i += len;
                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).ok_or("잘못된 유니코드 이스케이프")?
            }
            other => return Err(format!("알 수 없는 이스케이프 \\{other}")),
        })
    }

    fn literal_string(&mut self) -> Result<String, String> {
        self.i += 1;
        let start = self.i;
        while self.peek().is_some_and(|c| c != '\'' && c != '\n') {
            self.i += 1;
        }
        if self.peek() != Some('\'') {
            return Err("문자열이 닫히지 않았어요".into());
        }
        let s = self.c[start..self.i].iter().collect();
        self.i += 1;
        Ok(s)
    }

    fn skip_first_newline(&mut self) {
        if self.starts("\r\n") {
            self.i += 2;
        } else if self.peek() == Some('\n') {
            self.i += 1;
        }
    }

    fn multiline_basic(&mut self) -> Result<String, String> {
        self.i += 3;
        self.skip_first_newline();
        let mut out = String::new();
        loop {
            if self.starts("\"\"\"") && !self.starts("\"\"\"\"") {
                self.i += 3;
                return Ok(out);
            }
            match self.peek() {
                None => return Err("여러 줄 문자열이 닫히지 않았어요".into()),
                Some('\\') => {
                    self.i += 1;
                    if matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                        // Line-ending backslash: drop the newline and the next line's indent.
                        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                            self.i += 1;
                        }
                    } else {
                        out.push(self.escape()?);
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn multiline_literal(&mut self) -> Result<String, String> {
        self.i += 3;
        self.skip_first_newline();
        let start = self.i;
        while !(self.starts("'''") && !self.starts("''''")) {
            if self.peek().is_none() {
                return Err("여러 줄 문자열이 닫히지 않았어요".into());
            }
            self.i += 1;
        }
        let s = self.c[start..self.i].iter().collect();
        self.i += 3;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_codex_style_mcp_tables() {
        let src = r#"
model = "o3" # trailing comment
[features]
x = true

[mcp_servers.memory]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory",
        # comment inside an array
        "--flag",]
env = { API_KEY = "s3cret", "QUOTED.KEY" = 'lit' }

[mcp_servers."my.server"]
url = "https://example.com/mcp"
enabled = false
startup_timeout_sec = 10.5

[mcp_servers.memory.env_http_headers]
X-Token = "TOKEN_VAR"

[[profiles.list]]
name = "a"
[[profiles.list]]
name = "b"
desc = """
two
lines \
  joined"""
raw = '''C:\path'''
dotted.inner.key = 1_000
"#;
        let v = parse(src).unwrap();
        assert_eq!(v["model"], json!("o3"));
        let m = &v["mcp_servers"]["memory"];
        assert_eq!(m["command"], json!("npx"));
        assert_eq!(m["args"], json!(["-y", "@modelcontextprotocol/server-memory", "--flag"]));
        assert_eq!(m["env"]["API_KEY"], json!("s3cret"));
        assert_eq!(m["env"]["QUOTED.KEY"], json!("lit"));
        assert_eq!(m["env_http_headers"]["X-Token"], json!("TOKEN_VAR"));
        assert_eq!(v["mcp_servers"]["my.server"]["enabled"], json!(false));
        assert_eq!(v["mcp_servers"]["my.server"]["startup_timeout_sec"], json!(10.5));
        assert_eq!(v["profiles"]["list"][1]["desc"], json!("two\nlines joined"));
        assert_eq!(v["profiles"]["list"][1]["raw"], json!("C:\\path"));
        assert_eq!(v["profiles"]["list"][1]["dotted"]["inner"]["key"], json!(1000));
        assert_eq!(v["profiles"]["list"][0]["name"], json!("a"));
    }

    #[test]
    fn rejects_broken_files() {
        assert!(parse("a = \"open").is_err());
        assert!(parse("[table").is_err());
        assert!(parse("a = [1, 2").is_err());
        assert!(parse("a = 1 b = 2").is_err());
    }
}
