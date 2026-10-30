//! Parser (spec §3): a byte range of source text → an [`Ast`], a flat node
//! table. Recursive descent over the grammar, one function per precedence
//! level.
//!
//! The table is SoA with u32 child indices. Children are always created
//! before their parent, so node order is a valid post-order. Parentheses make
//! no node. Recursion is bounded by [`MAX_DEPTH`]; long left-associative
//! chains (`a+b+c+…`) loop instead of recursing, so they are bounded only by
//! [`MAX_NODES`].
//!
//! Contents:
//! - `pub(crate) enum BinOp`, `pub(crate) enum Node`, `pub(crate) struct Ast`
//! - `pub(crate) fn parse(src: &str, start: usize, end: usize) -> Result<Ast, Error>`
//! - `Ast::str_lit(&self, n: usize) -> &str`,
//!   `Ast::text<'source>(&self, src: &'source str, n: usize) -> &'source str`

use crate::lex::{self, Token, TokenKind};
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

struct Parser<'source> {
    src: &'source str,
    tok: Token,
    depth: u32,
    /// Call arguments being collected; nested calls share it as a stack.
    args: Vec<u32>,
    ast: Ast,
}

/// Parses `src[start..end]`. Spans are absolute offsets into `src`, so a
/// template hole reports positions in the whole template.
pub(crate) fn parse(src: &str, start: usize, end: usize) -> Result<Ast, Error> {
    assert!(start <= end, "parse range {start}..{end} is reversed");
    assert!(
        end <= src.len(),
        "parse range {start}..{end} past the source's {} bytes",
        src.len()
    );
    let src = &src[..end];
    let mut ast = Ast::default();
    let tok = lex::next_token(src, start, &mut ast.pool)?;
    let mut p = Parser {
        src,
        tok,
        depth: 0,
        args: Vec::new(),
        ast,
    };
    let root = p.expr()?;
    if p.tok.kind != TokenKind::Eof {
        return Err(p.unexpected());
    }
    assert_eq!(p.depth, 0, "parser depth unbalanced");
    assert!(p.args.is_empty(), "call argument stack unbalanced");
    // Children precede parents, so the root is the last node.
    assert_eq!(
        root as usize + 1,
        p.ast.kind.len(),
        "the root is the last node"
    );
    p.ast.root = root;
    Ok(p.ast)
}

impl Ast {
    /// The unescaped text of string literal node `n`.
    pub(crate) fn str_lit(&self, n: usize) -> &str {
        assert_eq!(self.kind[n], Node::Str, "node {n} is not a string literal");
        let (start, len) = (self.a[n] as usize, self.b[n] as usize);
        debug_assert!(start + len <= self.pool.len(), "literal {n} past the pool");
        &self.pool[start..start + len]
    }

    /// The source text node `n` spans (for an identifier, its name).
    pub(crate) fn text<'source>(&self, src: &'source str, n: usize) -> &'source str {
        let span = self.span[n];
        debug_assert!(span.start < span.end, "node {n} spans no text");
        debug_assert!(span.end as usize <= src.len(), "node {n} past the source");
        &src[span.start as usize..span.end as usize]
    }
}

// ---- private ----

impl Parser<'_> {
    fn advance(&mut self) -> Result<(), Error> {
        debug_assert_ne!(self.tok.kind, TokenKind::Eof, "advanced past the end");
        let from = self.tok.span.end;
        self.tok = lex::next_token(self.src, from as usize, &mut self.ast.pool)?;
        debug_assert!(self.tok.span.start >= from, "tokens move forward");
        Ok(())
    }

    fn unexpected(&self) -> Error {
        let t = self.tok;
        let msg = if t.kind == TokenKind::Eof {
            "unexpected end of expression".to_string()
        } else {
            format!(
                "unexpected `{}`",
                &self.src[t.span.start as usize..t.span.end as usize]
            )
        };
        Error::new(Code::X009, t.span, msg)
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Result<(), Error> {
        debug_assert_ne!(
            kind,
            TokenKind::Eof,
            "the end is checked by `parse`, not expected"
        );
        if self.tok.kind != kind {
            let mut e = self.unexpected();
            e.message = format!("{}, expected {what}", e.message);
            return Err(e);
        }
        self.advance()
    }

    fn enter(&mut self) -> Result<(), Error> {
        debug_assert!(self.depth <= MAX_DEPTH, "depth checked on every entry");
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

    /// Leaves one nesting level entered with [`Parser::enter`].
    fn leave(&mut self) {
        assert!(self.depth > 0, "left a nesting level never entered");
        self.depth -= 1;
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
        debug_assert_eq!(self.ast.a.len(), id, "AST columns in step");
        debug_assert_eq!(self.ast.b.len(), id, "AST columns in step");
        debug_assert_eq!(self.ast.span.len(), id, "AST columns in step");
        self.ast.kind.push(kind);
        self.ast.a.push(a);
        self.ast.b.push(b);
        self.ast.span.push(span);
        debug_assert!(id < MAX_NODES);
        Ok(id as u32)
    }

    fn span_of(&self, n: u32) -> Span {
        let span = self.ast.span[n as usize];
        debug_assert!(span.start < span.end, "node {n} spans no text");
        span
    }

    fn bin(&mut self, op: BinOp, l: u32, r: u32) -> Result<u32, Error> {
        debug_assert!(l < r, "the left operand is built first");
        debug_assert!((r as usize) < self.ast.kind.len(), "operands exist");
        let span = self.span_of(l).join(self.span_of(r));
        self.node(Node::Bin(op), l, r, span)
    }

    // expr = or ["?" expr ":" expr]
    fn expr(&mut self) -> Result<u32, Error> {
        self.enter()?;
        let cond = self.or()?;
        let out = if self.tok.kind == TokenKind::Question {
            self.advance()?;
            let then = self.expr()?;
            self.expect(TokenKind::Colon, "`:`")?;
            let else_arm = self.expr()?;
            // Post-order: the arms are built in source order, after the condition.
            debug_assert!(cond < then, "the condition precedes its arms");
            debug_assert!(then < else_arm, "the then-arm precedes the else-arm");
            let x = self.ast.extra.len() as u32;
            self.ast.extra.extend([then, else_arm]);
            let span = self.span_of(cond).join(self.span_of(else_arm));
            self.node(Node::Ternary, cond, x, span)?
        } else {
            cond
        };
        self.leave();
        Ok(out)
    }

    fn or(&mut self) -> Result<u32, Error> {
        let mut l = self.and()?;
        while self.tok.kind == TokenKind::OrOr {
            self.advance()?;
            let r = self.and()?;
            l = self.bin(BinOp::Or, l, r)?;
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<u32, Error> {
        let mut l = self.cmp()?;
        while self.tok.kind == TokenKind::AndAnd {
            self.advance()?;
            let r = self.cmp()?;
            l = self.bin(BinOp::And, l, r)?;
        }
        Ok(l)
    }

    fn cmp_op(&self) -> Option<BinOp> {
        Some(match self.tok.kind {
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::NotEq => BinOp::Ne,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::Le => BinOp::Le,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::Ge => BinOp::Ge,
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
            e.message = format!("{}: comparisons do not chain", e.message);
            return Err(e);
        }
        self.bin(op, l, r)
    }

    fn add(&mut self) -> Result<u32, Error> {
        let mut l = self.mul()?;
        loop {
            let op = match self.tok.kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
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
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::Percent => BinOp::Rem,
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
            TokenKind::Minus => Node::Neg,
            TokenKind::Bang => Node::Not,
            _ => return self.pow(),
        };
        let start = self.tok.span;
        self.advance()?;
        self.enter()?;
        let c = self.unary()?;
        self.leave();
        let span = start.join(self.span_of(c));
        self.node(kind, c, 0, span)
    }

    // pow = call ["^" unary]   (right-associative through unary → pow)
    fn pow(&mut self) -> Result<u32, Error> {
        let base = self.primary()?;
        if self.tok.kind != TokenKind::Caret {
            return Ok(base);
        }
        self.advance()?;
        self.enter()?;
        let e = self.unary()?;
        self.leave();
        self.bin(BinOp::Pow, base, e)
    }

    // primary = number | string | ident | ns "." ident | "(" expr ")", with
    // an optional call suffix on identifiers.
    fn primary(&mut self) -> Result<u32, Error> {
        let t = self.tok;
        debug_assert!(
            self.ast.ints.len() + self.ast.nums.len() <= self.ast.kind.len(),
            "every literal has its node"
        );
        let (kind, a, b) = match t.kind {
            TokenKind::Int(v) => {
                self.ast.ints.push(v);
                (Node::Int, self.ast.ints.len() as u32 - 1, 0)
            }
            TokenKind::Num(v) => {
                self.ast.nums.push(v);
                (Node::Num, self.ast.nums.len() as u32 - 1, 0)
            }
            TokenKind::Str { start, len } => (Node::Str, start, len),
            TokenKind::True => (Node::True, 0, 0),
            TokenKind::False => (Node::False, 0, 0),
            TokenKind::Pi => (Node::Pi, 0, 0),
            TokenKind::Ident | TokenKind::NsIdent => {
                self.advance()?;
                if self.tok.kind == TokenKind::LParen {
                    return self.call(t);
                }
                let kind = if t.kind == TokenKind::Ident {
                    Node::Ident
                } else {
                    Node::NsIdent
                };
                return self.node(kind, 0, 0, t.span);
            }
            TokenKind::LParen => {
                self.advance()?;
                let e = self.expr()?;
                self.expect(TokenKind::RParen, "`)`")?;
                return Ok(e);
            }
            _ => return Err(self.unexpected()),
        };
        self.advance()?;
        self.node(kind, a, b, t.span)
    }

    /// `name(args…)`; the current token is `(`.
    fn call(&mut self, name: Token) -> Result<u32, Error> {
        debug_assert_eq!(self.tok.kind, TokenKind::LParen);
        self.advance()?;
        let base = self.args.len();
        if self.tok.kind != TokenKind::RParen {
            loop {
                let arg = self.expr()?;
                self.args.push(arg);
                if self.tok.kind != TokenKind::Comma {
                    break;
                }
                self.advance()?;
            }
        }
        let close = self.tok.span;
        self.expect(TokenKind::RParen, "`,` or `)`")?;
        let x = self.ast.extra.len() as u32;
        let argc = (self.args.len() - base) as u32;
        self.ast.extra.push(name.span.end);
        self.ast.extra.extend(self.args.drain(base..));
        debug_assert_eq!(self.args.len(), base, "nested calls restore the stack");
        debug_assert_eq!(self.ast.extra.len(), x as usize + 1 + argc as usize);
        let kind = if name.kind == TokenKind::Ident {
            Node::Call
        } else {
            Node::NsCall
        };
        self.node(kind, x, argc, name.span.join(close))
    }
}
