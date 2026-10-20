//! The ATerm text of a `.drv` file, as plain strings.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Error, Result};

pub struct Parts {
    /// `[name, path, hash algo, hash]`
    pub outputs: Vec<[String; 4]>,
    pub input_drvs: BTreeMap<String, BTreeSet<String>>,
    pub input_srcs: Vec<String>,
    pub system: String,
    pub builder: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

pub fn write(p: &Parts) -> String {
    let mut out = String::from("Derive([");
    list(&mut out, &p.outputs, |out, output| {
        out.push('(');
        list(out, output, |out, s| string(out, s));
        out.push(')');
    });
    out.push_str("],[");
    list(&mut out, &p.input_drvs, |out, (drv, outputs)| {
        out.push('(');
        string(out, drv);
        out.push_str(",[");
        list(out, outputs, |out, s| string(out, s));
        out.push_str("])");
    });
    out.push_str("],[");
    list(&mut out, &p.input_srcs, |out, s| string(out, s));
    out.push_str("],");
    string(&mut out, &p.system);
    out.push(',');
    string(&mut out, &p.builder);
    out.push_str(",[");
    list(&mut out, &p.args, |out, s| string(out, s));
    out.push_str("],[");
    list(&mut out, &p.env, |out, (name, value)| {
        out.push('(');
        string(out, name);
        out.push(',');
        string(out, value);
        out.push(')');
    });
    out.push_str("])");
    out
}

fn list<T>(
    out: &mut String,
    items: impl IntoIterator<Item = T>,
    mut item: impl FnMut(&mut String, T),
) {
    for (i, x) in items.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        item(out, x);
    }
}

fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn parse(s: &str) -> Result<Parts> {
    let mut p = Parser {
        s: s.as_bytes(),
        pos: 0,
    };
    p.expect("Derive([")?;
    let outputs = p.list(|p| {
        p.expect("(")?;
        let name = p.string()?;
        p.expect(",")?;
        let path = p.string()?;
        p.expect(",")?;
        let algo = p.string()?;
        p.expect(",")?;
        let hash = p.string()?;
        p.expect(")")?;
        Ok([name, path, algo, hash])
    })?;
    p.expect(",[")?;
    let mut input_drvs = BTreeMap::new();
    for (drv, outputs) in p.list(|p| {
        p.expect("(")?;
        let drv = p.string()?;
        p.expect(",[")?;
        let outputs = p.list(Parser::string)?;
        p.expect(")")?;
        Ok((drv, outputs.into_iter().collect::<BTreeSet<_>>()))
    })? {
        input_drvs.insert(drv, outputs);
    }
    p.expect(",[")?;
    let input_srcs = p.list(Parser::string)?;
    p.expect(",")?;
    let system = p.string()?;
    p.expect(",")?;
    let builder = p.string()?;
    p.expect(",[")?;
    let args = p.list(Parser::string)?;
    p.expect(",[")?;
    let env = p
        .list(|p| {
            p.expect("(")?;
            let name = p.string()?;
            p.expect(",")?;
            let value = p.string()?;
            p.expect(")")?;
            Ok((name, value))
        })?
        .into_iter()
        .collect();
    p.expect(")")?;
    if p.pos != p.s.len() {
        return Err(p.error("data after the end"));
    }
    Ok(Parts {
        outputs,
        input_drvs,
        input_srcs,
        system,
        builder,
        args,
        env,
    })
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> Error {
        Error::Invalid(format!("{what} at byte {}", self.pos))
    }

    fn expect(&mut self, token: &str) -> Result<()> {
        if self.s[self.pos..].starts_with(token.as_bytes()) {
            self.pos += token.len();
            Ok(())
        } else {
            Err(self.error(&format!("expected {token:?}")))
        }
    }

    /// Items up to and including the closing `]` (the `[` is already read).
    fn list<T>(&mut self, mut item: impl FnMut(&mut Self) -> Result<T>) -> Result<Vec<T>> {
        let mut items = Vec::new();
        if self.expect("]").is_ok() {
            return Ok(items);
        }
        loop {
            items.push(item(self)?);
            if self.expect("]").is_ok() {
                return Ok(items);
            }
            self.expect(",")?;
        }
    }

    fn string(&mut self) -> Result<String> {
        self.expect("\"")?;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.pos) else {
                return Err(self.error("unterminated string"));
            };
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.pos) else {
                        return Err(self.error("unterminated string"));
                    };
                    self.pos += 1;
                    out.push(match e {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        e => e,
                    });
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.error("string is not UTF-8"))
    }
}
