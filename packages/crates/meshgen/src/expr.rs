//! A tiny self-contained arithmetic **expression evaluator** for formula
//! displacement (`Modifier::Displace`). Compiles once to an AST, then evaluates
//! per vertex against a [`Vars`] context.
//!
//! Grammar (recursive descent): `+ - * /`, unary `-`, parentheses, decimal
//! literals, the named variables in [`Vars`], and functions — 1-arg
//! (`sin cos tan abs sqrt floor fract sign exp log`), 2-arg
//! (`min max pow mod atan2 step`), 3-arg (`clamp(v, lo, hi)`), and
//! `noise(x, y)` / `noise(x, y, z)`.
//!
//! `noise` is deterministic smooth value noise in `[-1, 1]` (hash-lattice +
//! smoothstep) — the generic terrain primitive: the agent composes
//! fbm / ridged / domain-warp by SUMMING octaves itself (e.g.
//! `noise(x*4,z*4)*0.5 + noise(x*8,z*8)*0.25`), so there is no fixed terrain
//! menu — just the one primitive. No external deps — deliberately minimal.

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
    Call(String, Vec<Ast>),
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
        Ast::Call(f, args) => {
            // Evaluate every argument first (all funcs are strict).
            let mut a = [0f32; 3];
            for (slot, e) in a.iter_mut().zip(args) {
                *slot = eval_ast(e, vars)?;
            }
            match (f.as_str(), args.len()) {
                ("sin", 1) => a[0].sin(),
                ("cos", 1) => a[0].cos(),
                ("tan", 1) => a[0].tan(),
                ("abs", 1) => a[0].abs(),
                ("sqrt", 1) => a[0].sqrt(),
                ("floor", 1) => a[0].floor(),
                ("fract", 1) => a[0].fract(),
                ("sign", 1) => a[0].signum(),
                ("exp", 1) => a[0].exp(),
                ("log", 1) => a[0].ln(),
                ("min", 2) => a[0].min(a[1]),
                ("max", 2) => a[0].max(a[1]),
                ("pow", 2) => a[0].powf(a[1]),
                ("mod", 2) => a[0].rem_euclid(a[1]),
                ("atan2", 2) => a[0].atan2(a[1]),
                ("step", 2) => {
                    if a[1] < a[0] {
                        0.0
                    } else {
                        1.0
                    }
                }
                ("clamp", 3) => a[0].clamp(a[1], a[2]),
                ("noise", 2) => noise2(a[0], a[1]),
                ("noise", 3) => noise3(a[0], a[1], a[2]),
                _ => return None, // unknown function or wrong arity
            }
        }
    })
}

/// Deterministic integer-lattice hash → `[-1, 1]` (no RNG; stable native + wasm).
fn hash3(x: i32, y: i32, z: i32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374_761_393)
        .wrapping_add((y as u32).wrapping_mul(668_265_263))
        .wrapping_add((z as u32).wrapping_mul(1_274_126_177));
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    h ^= h >> 16;
    (h as f32 / u32::MAX as f32) * 2.0 - 1.0
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Smooth value noise in `[-1, 1]` over a 2D lattice (z fixed at 0).
fn noise2(x: f32, y: f32) -> f32 {
    noise3(x, y, 0.0)
}

/// Smooth value noise in `[-1, 1]` over a 3D lattice (trilinear smoothstep).
fn noise3(x: f32, y: f32, z: f32) -> f32 {
    let (xi, yi, zi) = (x.floor(), y.floor(), z.floor());
    let (xf, yf, zf) = (x - xi, y - yi, z - zi);
    let (x0, y0, z0) = (xi as i32, yi as i32, zi as i32);
    let (u, v, w) = (smoothstep(xf), smoothstep(yf), smoothstep(zf));
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    // 8 corners → trilinear interpolate.
    let c000 = hash3(x0, y0, z0);
    let c100 = hash3(x0 + 1, y0, z0);
    let c010 = hash3(x0, y0 + 1, z0);
    let c110 = hash3(x0 + 1, y0 + 1, z0);
    let c001 = hash3(x0, y0, z0 + 1);
    let c101 = hash3(x0 + 1, y0, z0 + 1);
    let c011 = hash3(x0, y0 + 1, z0 + 1);
    let c111 = hash3(x0 + 1, y0 + 1, z0 + 1);
    let x00 = lerp(c000, c100, u);
    let x10 = lerp(c010, c110, u);
    let x01 = lerp(c001, c101, u);
    let x11 = lerp(c011, c111, u);
    let y0_ = lerp(x00, x10, v);
    let y1_ = lerp(x01, x11, v);
    lerp(y0_, y1_, w)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f32),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
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
                ',' => out.push(Tok::Comma),
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
                // A function call if followed by '(' — parse a comma-separated
                // argument list (1..=3 args; arity is checked at eval time).
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.pos += 1;
                    let mut args = vec![self.parse_expr()?];
                    while matches!(self.peek(), Some(Tok::Comma)) {
                        self.pos += 1;
                        args.push(self.parse_expr()?);
                    }
                    matches!(self.peek(), Some(Tok::RParen)).then_some(())?;
                    self.pos += 1;
                    if args.len() > 3 {
                        return None; // no function takes > 3 args
                    }
                    Some(Ast::Call(name, args))
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

    #[test]
    fn multi_arg_functions() {
        let v = Vars::default();
        assert_eq!(eval("min(3, 5)", &v), Some(3.0));
        assert_eq!(eval("max(3, 5)", &v), Some(5.0));
        assert_eq!(eval("pow(2, 10)", &v), Some(1024.0));
        assert_eq!(eval("clamp(9, 0, 5)", &v), Some(5.0));
        assert_eq!(eval("mod(7, 3)", &v), Some(1.0));
        assert_eq!(eval("step(0.5, 0.7)", &v), Some(1.0));
        // Wrong arity / unknown fn fails to evaluate (loud reject upstream).
        assert!(eval("min(1)", &v).is_none());
        assert!(eval("noise(1)", &v).is_none());
        assert!(eval("sin(1, 2)", &v).is_none());
    }

    #[test]
    fn noise_is_deterministic_smooth_and_bounded() {
        let v = Vars::default();
        // Deterministic: same inputs → same output (no RNG).
        let a = eval("noise(1.5, 2.5)", &v).unwrap();
        let b = eval("noise(1.5, 2.5)", &v).unwrap();
        assert_eq!(a, b);
        // Bounded in [-1, 1] across a sweep; not flat (varies).
        let mut min = f32::MAX;
        let mut max = f32::MIN;
        for k in 0..50 {
            let t = k as f32 * 0.37;
            let n = noise2(t, t * 0.5);
            assert!((-1.0..=1.0).contains(&n), "noise out of range: {n}");
            min = min.min(n);
            max = max.max(n);
        }
        assert!(max - min > 0.3, "noise looks flat: {min}..{max}");
        // 3D noise compiles + evaluates.
        assert!(eval("noise(x, y, z)", &v).is_some());
        // fbm-by-summed-octaves (the intended composition) parses + evaluates.
        assert!(eval("noise(x*4, z*4)*0.5 + noise(x*8, z*8)*0.25", &v).is_some());
    }
}
