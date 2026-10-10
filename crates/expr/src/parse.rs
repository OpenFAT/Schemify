//! Parser (spec §3): recursive descent over the grammar, one function per
//! precedence level, into a flat node table.
//!
//! The table is SoA with u32 child indices. Children are always created
//! before their parent, so node order is a valid post-order. Parentheses make
//! no node. Recursion is bounded by [`MAX_DEPTH`]; long left-associative
//! chains (`a+b+c+…`) loop instead of recursing, so they are bounded only by
//! [`MAX_NODES`].

use crate::lex::{Lit, Tok, Token, next_token};
use crate::{Code, Error, MAX_DEPTH, MAX_NODES, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

/// Node kind. Payload columns `a`/`b` by kind:
/// `Int`/`Num`: a = index into `ints`/`nums`. `Str`: a, b = pool start, len.
/// `Ident`/`NsIdent`: name is the span text. `Neg`/`Not`: a = child.
/// `Bin`: a, b = children. `Ternary`: a = cond, b = index into `extra` of
/// `[then, else]`. `Call`/`NsCall`: a = index into `extra` of
/// `[name_end, args…]`, b = argc; the callee name is `src[span.start..name_end]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    Int,
    Num,
    Str,
    True,
    False,
    Pi,
    Ident,
    NsIdent,
    Neg,
    Not,
    Bin(BinOp),
    Ternary,
    Call,
    NsCall,
}

/// One parsed expression. Lifetime: one compile call.
#[derive(Debug, Default)]
pub(crate) struct Ast {
    pub kind: Vec<Node>,
    pub a: Vec<u32>,
    pub b: Vec<u32>,
    pub span: Vec<Span>,
    pub extra: Vec<u32>,
    pub ints: Vec<i64>,
    pub nums: Vec<f64>,
    /// Unescaped string literals.
    pub pool: String,
    pub root: u32,
}

struct Parser<'s> {
    src: &'s str,
    tok: Token,
    depth: u32,
    /// Call arguments being collected; nested calls share it as a stack.
    args: Vec<u32>,
    ast: Ast,
}

/// Parses `src[start..end]`. Spans are absolute offsets into `src`, so a
/// template hole reports positions in the whole template.
pub(crate) fn parse(src: &str, start: usize, end: usize) -> Result<Ast, Error> {
    assert!(
        start <= end && end <= src.len(),
        "parse range {start}..{end} outside source"
    );
    let src = &src[..end];
    let mut ast = Ast::default();
    let tok = next_token(src, start, &mut ast.pool)?;
    let mut p = Parser {
        src,
        tok,
        depth: 0,
        args: Vec::new(),
        ast,
    };
    let root = p.expr()?;
    if p.tok.kind != Tok::Eof {
        return Err(p.unexpected());
    }
    assert_eq!(p.depth, 0, "parser depth unbalanced");
    assert!(p.args.is_empty(), "call argument stack unbalanced");
    p.ast.root = root;
    Ok(p.ast)
}

impl Parser<'_> {
    fn advance(&mut self) -> Result<(), Error> {
        self.tok = next_token(self.src, self.tok.span.end as usize, &mut self.ast.pool)?;
        Ok(())
    }

    fn unexpected(&self) -> Error {
        let t = self.tok;
        let msg = if t.kind == Tok::Eof {
            "unexpected end of expression".to_string()
        } else {
            format!(
                "unexpected `{}`",
                &self.src[t.span.start as usize..t.span.end as usize]
            )
        };
        Error::new(Code::X009, t.span, msg)
    }

    fn expect(&mut self, kind: Tok, what: &str) -> Result<(), Error> {
        if self.tok.kind != kind {
            let mut e = self.unexpected();
            e.msg = format!("{}, expected {what}", e.msg);
            return Err(e);
        }
        self.advance()
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Error::new(
                Code::X002,
                self.tok.span,
                format!("expression nested deeper than {MAX_DEPTH}"),
            ));
        }
        Ok(())
    }

    fn node(&mut self, kind: Node, a: u32, b: u32, span: Span) -> Result<u32, Error> {
        let id = self.ast.kind.len();
        if id == MAX_NODES {
            return Err(Error::new(
                Code::X002,
                span,
                format!("expression has more than {MAX_NODES} nodes"),
            ));
        }
        debug_assert!(
            self.ast.a.len() == id && self.ast.b.len() == id && self.ast.span.len() == id
        );
        self.ast.kind.push(kind);
        self.ast.a.push(a);
        self.ast.b.push(b);
        self.ast.span.push(span);
        Ok(id as u32)
    }

    fn span_of(&self, n: u32) -> Span {
        self.ast.span[n as usize]
    }

    fn bin(&mut self, op: BinOp, l: u32, r: u32) -> Result<u32, Error> {
        let span = self.span_of(l).join(self.span_of(r));
        self.node(Node::Bin(op), l, r, span)
    }

    // expr = or ["?" expr ":" expr]
    fn expr(&mut self) -> Result<u32, Error> {
        self.enter()?;
        let cond = self.or()?;
        let out = if self.tok.kind == Tok::Question {
            self.advance()?;
            let then = self.expr()?;
            self.expect(Tok::Colon, "`:`")?;
            let els = self.expr()?;
            let x = self.ast.extra.len() as u32;
            self.ast.extra.extend([then, els]);
            let span = self.span_of(cond).join(self.span_of(els));
            self.node(Node::Ternary, cond, x, span)?
        } else {
            cond
        };
        self.depth -= 1;
        Ok(out)
    }

    fn or(&mut self) -> Result<u32, Error> {
        let mut l = self.and()?;
        while self.tok.kind == Tok::OrOr {
            self.advance()?;
            let r = self.and()?;
            l = self.bin(BinOp::Or, l, r)?;
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<u32, Error> {
        let mut l = self.cmp()?;
        while self.tok.kind == Tok::AndAnd {
            self.advance()?;
            let r = self.cmp()?;
            l = self.bin(BinOp::And, l, r)?;
        }
        Ok(l)
    }

    fn cmp_op(&self) -> Option<BinOp> {
        Some(match self.tok.kind {
            Tok::EqEq => BinOp::Eq,
            Tok::NotEq => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            _ => return None,
        })
    }

    // Non-associative: `a < b < c` is a parse error.
    fn cmp(&mut self) -> Result<u32, Error> {
        let l = self.add()?;
        let Some(op) = self.cmp_op() else {
            return Ok(l);
        };
        self.advance()?;
        let r = self.add()?;
        if self.cmp_op().is_some() {
            let mut e = self.unexpected();
            e.msg = format!("{}: comparisons do not chain", e.msg);
            return Err(e);
        }
        self.bin(op, l, r)
    }

    fn add(&mut self) -> Result<u32, Error> {
        let mut l = self.mul()?;
        loop {
            let op = match self.tok.kind {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(l),
            };
            self.advance()?;
            let r = self.mul()?;
            l = self.bin(op, l, r)?;
        }
    }

    fn mul(&mut self) -> Result<u32, Error> {
        let mut l = self.unary()?;
        loop {
            let op = match self.tok.kind {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => return Ok(l),
            };
            self.advance()?;
            let r = self.unary()?;
            l = self.bin(op, l, r)?;
        }
    }

    // unary = ("-" | "!") unary | pow
    fn unary(&mut self) -> Result<u32, Error> {
        let kind = match self.tok.kind {
            Tok::Minus => Node::Neg,
            Tok::Bang => Node::Not,
            _ => return self.pow(),
        };
        let start = self.tok.span;
        self.advance()?;
        self.enter()?;
        let c = self.unary()?;
        self.depth -= 1;
        let span = start.join(self.span_of(c));
        self.node(kind, c, 0, span)
    }

    // pow = call ["^" unary]   (right-associative through unary → pow)
    fn pow(&mut self) -> Result<u32, Error> {
        let base = self.primary()?;
        if self.tok.kind != Tok::Caret {
            return Ok(base);
        }
        self.advance()?;
        self.enter()?;
        let e = self.unary()?;
        self.depth -= 1;
        self.bin(BinOp::Pow, base, e)
    }

    // primary = number | string | ident | ns "." ident | "(" expr ")", with
    // an optional call suffix on identifiers.
    fn primary(&mut self) -> Result<u32, Error> {
        let t = self.tok;
        let leaf = match (t.kind, t.lit) {
            (Tok::Int, Lit::Int(v)) => {
                self.ast.ints.push(v);
                (Node::Int, self.ast.ints.len() as u32 - 1, 0)
            }
            (Tok::Num, Lit::Num(v)) => {
                self.ast.nums.push(v);
                (Node::Num, self.ast.nums.len() as u32 - 1, 0)
            }
            (Tok::Str, Lit::Str { start, len }) => (Node::Str, start, len),
            (Tok::True, _) => (Node::True, 0, 0),
            (Tok::False, _) => (Node::False, 0, 0),
            (Tok::Pi, _) => (Node::Pi, 0, 0),
            (Tok::Ident | Tok::NsIdent, _) => {
                self.advance()?;
                if self.tok.kind == Tok::LParen {
                    return self.call(t);
                }
                let kind = if t.kind == Tok::Ident {
                    Node::Ident
                } else {
                    Node::NsIdent
                };
                return self.node(kind, 0, 0, t.span);
            }
            (Tok::LParen, _) => {
                self.advance()?;
                let e = self.expr()?;
                self.expect(Tok::RParen, "`)`")?;
                return Ok(e);
            }
            _ => return Err(self.unexpected()),
        };
        self.advance()?;
        self.node(leaf.0, leaf.1, leaf.2, t.span)
    }

    /// `name(args…)`; the current token is `(`.
    fn call(&mut self, name: Token) -> Result<u32, Error> {
        debug_assert_eq!(self.tok.kind, Tok::LParen);
        self.advance()?;
        let base = self.args.len();
        if self.tok.kind != Tok::RParen {
            loop {
                let arg = self.expr()?;
                self.args.push(arg);
                if self.tok.kind != Tok::Comma {
                    break;
                }
                self.advance()?;
            }
        }
        let close = self.tok.span;
        self.expect(Tok::RParen, "`,` or `)`")?;
        let x = self.ast.extra.len() as u32;
        let argc = (self.args.len() - base) as u32;
        self.ast.extra.push(name.span.end);
        self.ast.extra.extend(self.args.drain(base..));
        let kind = if name.kind == Tok::Ident {
            Node::Call
        } else {
            Node::NsCall
        };
        self.node(kind, x, argc, name.span.join(close))
    }
}
