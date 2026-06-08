//! A tiny self-contained arithmetic **expression evaluator** for formula
//! displacement (`Modifier::Displace`). Compiles once to an AST, then evaluates
//! per vertex against a [`Vars`] context.
//!
//! Grammar (recursive descent): `+ - * /`, unary `-`, parentheses, decimal
//! literals, the named variables in [`Vars`], and unary functions
//! `sin cos tan abs sqrt floor sign`. No external deps — deliberately minimal
//! (the spec's "tiny shunting-yard" suggestion).

/// Per-vertex variables an expression may reference.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vars {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub nx: f32,
    pub ny: f32,
    pub nz: f32,
    pub u: f32,
    pub v: f32,
    /// Vertex index (as f32).
    pub i: f32,
}

impl Vars {
    fn get(&self, name: &str) -> Option<f32> {
        Some(match name {
            "x" => self.x,
            "y" => self.y,
            "z" => self.z,
            "nx" => self.nx,
            "ny" => self.ny,
            "nz" => self.nz,
            "u" => self.u,
            "v" => self.v,
            "i" => self.i,
            "pi" => std::f32::consts::PI,
            "tau" => std::f32::consts::TAU,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
enum Ast {
    Num(f32),
    Var(String),
    Neg(Box<Ast>),
    Bin(char, Box<Ast>, Box<Ast>),
    Call(String, Box<Ast>),
}

/// A parsed expression, ready to evaluate per vertex.
#[derive(Clone, Debug)]
pub struct Expr(Ast);

impl Expr {
    /// Parse `src`; `None` on a syntax error.
    pub fn compile(src: &str) -> Option<Expr> {
        let tokens = tokenize(src)?;
        let mut p = Parser { tokens, pos: 0 };
        let ast = p.parse_expr()?;
        if p.pos != p.tokens.len() {
            return None; // trailing garbage
        }
        Some(Expr(ast))
    }

    /// Evaluate against `vars`. `None` if an unknown variable/function is hit.
    pub fn eval(&self, vars: &Vars) -> Option<f32> {
        eval_ast(&self.0, vars)
    }
}

fn eval_ast(ast: &Ast, vars: &Vars) -> Option<f32> {
    Some(match ast {
        Ast::Num(n) => *n,
        Ast::Var(name) => vars.get(name)?,
        Ast::Neg(a) => -eval_ast(a, vars)?,
        Ast::Bin(op, a, b) => {
            let (a, b) = (eval_ast(a, vars)?, eval_ast(b, vars)?);
            match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                '/' => a / b,
                _ => return None,
            }
        }
        Ast::Call(f, a) => {
            let a = eval_ast(a, vars)?;
            match f.as_str() {
                "sin" => a.sin(),
                "cos" => a.cos(),
                "tan" => a.tan(),
                "abs" => a.abs(),
                "sqrt" => a.sqrt(),
                "floor" => a.floor(),
                "sign" => a.signum(),
                _ => return None,
            }
        }
    })
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f32),
    Ident(String),
    Op(char),
    LParen,
    RParen,
}

fn tokenize(src: &str) -> Option<Vec<Tok>> {
    let mut out = Vec::new();
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.') {
                i += 1;
            }
            let s: String = bytes[start..i].iter().collect();
            out.push(Tok::Num(s.parse().ok()?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == '_') {
                i += 1;
            }
            out.push(Tok::Ident(bytes[start..i].iter().collect()));
        } else {
            match c {
                '+' | '-' | '*' | '/' => out.push(Tok::Op(c)),
                '(' => out.push(Tok::LParen),
                ')' => out.push(Tok::RParen),
                _ => return None,
            }
            i += 1;
        }
    }
    Some(out)
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn parse_expr(&mut self) -> Option<Ast> {
        let mut lhs = self.parse_term()?;
        while let Some(Tok::Op(op @ ('+' | '-'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.parse_term()?;
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Some(lhs)
    }

    fn parse_term(&mut self) -> Option<Ast> {
        let mut lhs = self.parse_factor()?;
        while let Some(Tok::Op(op @ ('*' | '/'))) = self.peek().cloned() {
            self.pos += 1;
            let rhs = self.parse_factor()?;
            lhs = Ast::Bin(op, Box::new(lhs), Box::new(rhs));
        }
        Some(lhs)
    }

    fn parse_factor(&mut self) -> Option<Ast> {
        if let Some(Tok::Op('-')) = self.peek() {
            self.pos += 1;
            return Some(Ast::Neg(Box::new(self.parse_factor()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<Ast> {
        match self.peek().cloned()? {
            Tok::Num(n) => {
                self.pos += 1;
                Some(Ast::Num(n))
            }
            Tok::LParen => {
                self.pos += 1;
                let e = self.parse_expr()?;
                matches!(self.peek(), Some(Tok::RParen)).then_some(())?;
                self.pos += 1;
                Some(e)
            }
            Tok::Ident(name) => {
                self.pos += 1;
                // A function call if followed by '('.
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.pos += 1;
                    let arg = self.parse_expr()?;
                    matches!(self.peek(), Some(Tok::RParen)).then_some(())?;
                    self.pos += 1;
                    Some(Ast::Call(name, Box::new(arg)))
                } else {
                    Some(Ast::Var(name))
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str, vars: &Vars) -> Option<f32> {
        Expr::compile(src)?.eval(vars)
    }

    #[test]
    fn precedence_and_parens() {
        let v = Vars::default();
        assert_eq!(eval("1 + 2 * 3", &v), Some(7.0));
        assert_eq!(eval("(1 + 2) * 3", &v), Some(9.0));
        assert_eq!(eval("-2 + 5", &v), Some(3.0));
        assert_eq!(eval("8 / 2 / 2", &v), Some(2.0));
    }

    #[test]
    fn vars_and_functions() {
        let v = Vars {
            x: 3.0,
            y: 4.0,
            ..Default::default()
        };
        assert_eq!(eval("x * 2", &v), Some(6.0));
        assert_eq!(eval("sqrt(x*x + y*y)", &v), Some(5.0));
        assert!((eval("sin(0) + cos(0)", &v).unwrap() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Expr::compile("1 +").is_none());
        assert!(Expr::compile("(1 + 2").is_none());
        assert!(Expr::compile("1 ! 2").is_none());
        // Unknown variable parses but fails to evaluate.
        assert!(eval("bogus + 1", &Vars::default()).is_none());
    }
}
