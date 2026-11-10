//! A small interpreter for `ParameterCalculation` scripts.
//!
//! knxprod calculations are JavaScript, but the ones found in practice are a
//! handful of assignments with arithmetic, comparisons and `if`/`else`:
//!
//! ```js
//! p2 = p1 / p3;
//! if (p2 < 1)
//!   p2 = 1
//! ```
//!
//! This module implements exactly that subset over `f64` values: `var`
//! declarations, assignments, `if`/`else`, `switch` with `case`/`default`/
//! `break`, blocks, the operators
//! `|| && == != === !== < <= > >= + - * / %` and unary `- + !`, the ternary
//! `?:`, and `Math.{abs,floor,ceil,round,min,max,pow,sqrt,log,exp,trunc}`.
//! Anything else (loops, arrays, strings, functions) is rejected with an
//! error, so the caller can report the calculation as not evaluated instead
//! of producing a wrong value.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};

/// Run `script`, reading and writing `vars` (values are decimal strings).
pub fn run(script: &str, vars: &mut HashMap<String, String>) -> Result<()> {
    let tokens = lex(script)?;
    let mut p = Parser { tokens, pos: 0 };
    let program = p.program()?;
    let mut env: HashMap<String, f64> = HashMap::new();
    for (k, v) in vars.iter() {
        if let Ok(f) = v.trim().parse::<f64>() {
            env.insert(k.clone(), f);
        }
    }
    for stmt in &program {
        if exec(stmt, &mut env)? {
            bail!("`break` outside of a `switch`");
        }
    }
    for (k, v) in vars.iter_mut() {
        if let Some(f) = env.get(k) {
            *v = format_num(*f);
        }
    }
    Ok(())
}

fn format_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Op(&'static str),
}

const OPS: [&str; 24] = [
    "===", "!==", "==", "!=", "<=", ">=", "&&", "||", "<", ">", "+", "-", "*", "/", "%", "!", "=",
    "(", ")", "{", "}", ";", "?", ":",
];

fn lex(src: &str) -> Result<Vec<Tok>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if src[i..].starts_with("//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if src[i..].starts_with("/*") {
            let end = src[i + 2..].find("*/").ok_or_else(|| anyhow!("unterminated comment"))?;
            i += end + 4;
        } else if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            let start = i;
            if src[i..].starts_with("0x") || src[i..].starts_with("0X") {
                i += 2;
                while i < b.len() && (b[i] as char).is_ascii_hexdigit() {
                    i += 1;
                }
                let v = i64::from_str_radix(&src[start + 2..i], 16)?;
                out.push(Tok::Num(v as f64));
                continue;
            }
            while i < b.len() && ((b[i] as char).is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                i += 1;
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    i += 1;
                }
                while i < b.len() && (b[i] as char).is_ascii_digit() {
                    i += 1;
                }
            }
            out.push(Tok::Num(src[start..i].parse()?));
        } else if c.is_ascii_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] == b'.') {
                i += 1;
            }
            out.push(Tok::Ident(src[start..i].to_string()));
        } else if let Some(op) = OPS.iter().find(|op| src[i..].starts_with(**op)) {
            out.push(Tok::Op(op));
            i += op.len();
        } else if c == ',' {
            out.push(Tok::Op(","));
            i += 1;
        } else {
            bail!("unsupported character {c:?}");
        }
    }
    Ok(out)
}

#[derive(Debug)]
enum Stmt {
    Assign(String, Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Block(Vec<Stmt>),
    /// The value, and the `case` labels (`None` for `default`) with the
    /// statements up to the next label.
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Break,
    Expr(Expr),
    Empty,
}

#[derive(Debug)]
enum Expr {
    Num(f64),
    Var(String),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn peek_op(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if *o == op)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.peek_op(op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> Result<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            bail!("expected {op:?}, found {:?}", self.peek())
        }
    }

    fn program(&mut self) -> Result<Vec<Stmt>> {
        let mut out = Vec::new();
        while self.peek().is_some() {
            out.push(self.stmt()?);
        }
        Ok(out)
    }

    fn stmt(&mut self) -> Result<Stmt> {
        if self.eat_op(";") {
            return Ok(Stmt::Empty);
        }
        if self.eat_op("{") {
            let mut body = Vec::new();
            while !self.eat_op("}") {
                if self.peek().is_none() {
                    bail!("unterminated block");
                }
                body.push(self.stmt()?);
            }
            return Ok(Stmt::Block(body));
        }
        if let Some(Tok::Ident(word)) = self.peek().cloned() {
            match word.as_str() {
                "if" => {
                    self.pos += 1;
                    self.expect_op("(")?;
                    let cond = self.expr()?;
                    self.expect_op(")")?;
                    let then = Box::new(self.stmt()?);
                    let els = if matches!(self.peek(), Some(Tok::Ident(w)) if w == "else") {
                        self.pos += 1;
                        Some(Box::new(self.stmt()?))
                    } else {
                        None
                    };
                    return Ok(Stmt::If(cond, then, els));
                }
                "var" | "let" | "const" => {
                    self.pos += 1;
                    let Some(Tok::Ident(name)) = self.peek().cloned() else {
                        bail!("expected a variable name");
                    };
                    self.pos += 1;
                    let stmt = if self.eat_op("=") {
                        Stmt::Assign(name, self.expr()?)
                    } else {
                        Stmt::Empty
                    };
                    self.eat_op(";");
                    return Ok(stmt);
                }
                "switch" => {
                    self.pos += 1;
                    self.expect_op("(")?;
                    let value = self.expr()?;
                    self.expect_op(")")?;
                    self.expect_op("{")?;
                    let mut cases: Vec<(Option<Expr>, Vec<Stmt>)> = Vec::new();
                    while !self.eat_op("}") {
                        match self.peek().cloned() {
                            Some(Tok::Ident(w)) if w == "case" => {
                                self.pos += 1;
                                let label = self.expr()?;
                                self.expect_op(":")?;
                                cases.push((Some(label), Vec::new()));
                            }
                            Some(Tok::Ident(w)) if w == "default" => {
                                self.pos += 1;
                                self.expect_op(":")?;
                                cases.push((None, Vec::new()));
                            }
                            None => bail!("unterminated switch"),
                            _ => {
                                let stmt = self.stmt()?;
                                let Some((_, body)) = cases.last_mut() else {
                                    bail!("statement before the first case");
                                };
                                body.push(stmt);
                            }
                        }
                    }
                    return Ok(Stmt::Switch(value, cases));
                }
                "break" => {
                    self.pos += 1;
                    self.eat_op(";");
                    return Ok(Stmt::Break);
                }
                "for" | "while" | "do" | "function" | "return" => {
                    bail!("unsupported statement `{word}`");
                }
                _ => {
                    if matches!(self.tokens.get(self.pos + 1), Some(Tok::Op("="))) {
                        self.pos += 2;
                        let e = self.expr()?;
                        self.eat_op(";");
                        return Ok(Stmt::Assign(word, e));
                    }
                }
            }
        }
        let e = self.expr()?;
        self.eat_op(";");
        Ok(Stmt::Expr(e))
    }

    fn expr(&mut self) -> Result<Expr> {
        let cond = self.binary(0)?;
        if self.eat_op("?") {
            let a = self.expr()?;
            self.expect_op(":")?;
            let b = self.expr()?;
            return Ok(Expr::Cond(Box::new(cond), Box::new(a), Box::new(b)));
        }
        Ok(cond)
    }

    fn binary(&mut self, level: usize) -> Result<Expr> {
        const LEVELS: [&[&str]; 6] = [
            &["||"],
            &["&&"],
            &["===", "!==", "==", "!="],
            &["<=", ">=", "<", ">"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        loop {
            let Some(Tok::Op(op)) = self.peek().cloned() else { break };
            if !LEVELS[level].contains(&op) {
                break;
            }
            self.pos += 1;
            let rhs = self.binary(level + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr> {
        for op in ["-", "+", "!"] {
            if self.eat_op(op) {
                let op: &'static str = match op {
                    "-" => "-",
                    "+" => "+",
                    _ => "!",
                };
                return Ok(Expr::Unary(op, Box::new(self.unary()?)));
            }
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.peek().cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                if self.eat_op("(") {
                    let mut args = Vec::new();
                    if !self.eat_op(")") {
                        loop {
                            args.push(self.expr()?);
                            if self.eat_op(")") {
                                break;
                            }
                            self.expect_op(",")?;
                        }
                    }
                    return Ok(Expr::Call(name, args));
                }
                match name.as_str() {
                    "true" => Ok(Expr::Num(1.0)),
                    "false" => Ok(Expr::Num(0.0)),
                    _ => Ok(Expr::Var(name)),
                }
            }
            Some(Tok::Op("(")) => {
                self.pos += 1;
                let e = self.expr()?;
                self.expect_op(")")?;
                Ok(e)
            }
            other => bail!("unexpected {other:?}"),
        }
    }
}

/// Run a statement; `true` when it ended with `break`.
fn exec(stmt: &Stmt, env: &mut HashMap<String, f64>) -> Result<bool> {
    match stmt {
        Stmt::Assign(name, e) => {
            let v = eval(e, env)?;
            env.insert(name.clone(), v);
        }
        Stmt::If(c, a, b) => {
            if eval(c, env)? != 0.0 {
                return exec(a, env);
            } else if let Some(b) = b {
                return exec(b, env);
            }
        }
        Stmt::Block(body) => {
            for s in body {
                if exec(s, env)? {
                    return Ok(true);
                }
            }
        }
        Stmt::Switch(value, cases) => {
            // JavaScript compares the labels with `===`; all values here are
            // numbers. Execution falls through to the following cases until
            // a `break`.
            let v = eval(value, env)?;
            let mut start = None;
            for (i, (label, _)) in cases.iter().enumerate() {
                if let Some(l) = label {
                    if eval(l, env)? == v {
                        start = Some(i);
                        break;
                    }
                }
            }
            let start = start.or_else(|| cases.iter().position(|(l, _)| l.is_none()));
            if let Some(start) = start {
                'cases: for (_, body) in &cases[start..] {
                    for s in body {
                        if exec(s, env)? {
                            break 'cases;
                        }
                    }
                }
            }
        }
        Stmt::Break => return Ok(true),
        Stmt::Expr(e) => {
            eval(e, env)?;
        }
        Stmt::Empty => {}
    }
    Ok(false)
}

fn eval(e: &Expr, env: &HashMap<String, f64>) -> Result<f64> {
    let b = |x: bool| if x { 1.0 } else { 0.0 };
    Ok(match e {
        Expr::Num(n) => *n,
        Expr::Var(v) => *env.get(v).ok_or_else(|| anyhow!("unknown variable {v}"))?,
        Expr::Unary(op, x) => {
            let x = eval(x, env)?;
            match *op {
                "-" => -x,
                "+" => x,
                _ => b(x == 0.0),
            }
        }
        Expr::Binary(op, l, r) => {
            let l = eval(l, env)?;
            // Short-circuit like JavaScript.
            if *op == "&&" {
                return Ok(if l == 0.0 { 0.0 } else { b(eval(r, env)? != 0.0) });
            }
            if *op == "||" {
                return Ok(if l != 0.0 { 1.0 } else { b(eval(r, env)? != 0.0) });
            }
            let r = eval(r, env)?;
            match *op {
                "+" => l + r,
                "-" => l - r,
                "*" => l * r,
                "/" => l / r,
                "%" => l % r,
                "==" | "===" => b(l == r),
                "!=" | "!==" => b(l != r),
                "<" => b(l < r),
                "<=" => b(l <= r),
                ">" => b(l > r),
                ">=" => b(l >= r),
                _ => bail!("operator {op}"),
            }
        }
        Expr::Cond(c, a, x) => {
            if eval(c, env)? != 0.0 {
                eval(a, env)?
            } else {
                eval(x, env)?
            }
        }
        Expr::Call(f, args) => {
            let a: Vec<f64> = args.iter().map(|x| eval(x, env)).collect::<Result<_>>()?;
            let one = || a.first().copied().ok_or_else(|| anyhow!("{f} needs an argument"));
            match f.as_str() {
                "Math.abs" => one()?.abs(),
                "Math.floor" => one()?.floor(),
                "Math.ceil" => one()?.ceil(),
                // JavaScript rounds .5 toward +∞.
                "Math.round" => (one()? + 0.5).floor(),
                "Math.trunc" => one()?.trunc(),
                "Math.sqrt" => one()?.sqrt(),
                "Math.log" => one()?.ln(),
                "Math.exp" => one()?.exp(),
                "Math.pow" if a.len() == 2 => a[0].powf(a[1]),
                "Math.min" if !a.is_empty() => a.iter().copied().fold(f64::INFINITY, f64::min),
                "Math.max" if !a.is_empty() => a.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                _ => bail!("unsupported function {f}"),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn mdt_counter_difference() {
        let script = "p2 = p1 / p3;\n\nif (p2 < 1)\n  p2 = 1\n\n\np4 = p1 / p3;\nif (p4 < 1)\n  p4 = 0";
        let mut v = vars(&[("p1", "2500"), ("p3", "1000"), ("p2", "0"), ("p4", "0")]);
        run(script, &mut v).unwrap();
        assert_eq!(v["p2"], "2.5");
        assert_eq!(v["p4"], "2.5");

        let mut v = vars(&[("p1", "10"), ("p3", "1000"), ("p2", "0"), ("p4", "0")]);
        run(script, &mut v).unwrap();
        assert_eq!(v["p2"], "1");
        assert_eq!(v["p4"], "0");
    }

    #[test]
    fn if_else_blocks_and_logic() {
        let script = "if (p1 == 2 || p2 ==2 ) {\nenable = 1;\n}\nelse {\nenable = 0;\n}";
        let mut v = vars(&[("p1", "0"), ("p2", "2"), ("enable", "0")]);
        run(script, &mut v).unwrap();
        assert_eq!(v["enable"], "1");
        let mut v = vars(&[("min", "0x12"), ("max", "30"), ("errorFlag", "5")]);
        // Hex literals in scripts, decimal values in variables.
        run("min = 0x12; if (min < max) { errorFlag = 0; } else { errorFlag = 1; }", &mut v).unwrap();
        assert_eq!(v["errorFlag"], "0");
    }

    /// The shape of MDT's `MappingError` calculations (BE-GT2Tx.02): a switch
    /// over a view mode with `if`/`else if` chains and `break`s.
    #[test]
    fn switch_with_cases_and_break() {
        let script = "switch (ViewMode)  {\n  case 0:\n    if ((p1 != 255) && (p1 == p2))\n      Error = 1\n    else\n      Error = 0\n    break;\n  case 1:\n    if ((p8 != 255) && (p8 == p9)) Error = 1\n    else Error = 0\n    break;\n  }";
        let mut v = vars(&[("ViewMode", "0"), ("p1", "3"), ("p2", "3"), ("p8", "1"), ("p9", "2"), ("Error", "9")]);
        run(script, &mut v).unwrap();
        assert_eq!(v["Error"], "1");
        v.insert("ViewMode".into(), "1".into());
        run(script, &mut v).unwrap();
        assert_eq!(v["Error"], "0");
        // No matching case and no default: nothing runs.
        v.insert("ViewMode".into(), "2".into());
        v.insert("Error".into(), "9".into());
        run(script, &mut v).unwrap();
        assert_eq!(v["Error"], "9");

        // Fall through without break, and default.
        let script = "switch (m) { case 1: a = 1; case 2: b = 2; break; default: a = 5; }";
        let mut v = vars(&[("m", "1"), ("a", "0"), ("b", "0")]);
        run(script, &mut v).unwrap();
        assert_eq!((v["a"].as_str(), v["b"].as_str()), ("1", "2"));
        let mut v = vars(&[("m", "7"), ("a", "0"), ("b", "0")]);
        run(script, &mut v).unwrap();
        assert_eq!((v["a"].as_str(), v["b"].as_str()), ("5", "0"));
        assert!(run("break;", &mut vars(&[])).is_err());
    }

    #[test]
    fn rejects_loops() {
        let mut v = vars(&[]);
        assert!(run("var a = []; for (i = 0; i < 3; i++) { a[i] = 0; }", &mut v).is_err());
    }
}
