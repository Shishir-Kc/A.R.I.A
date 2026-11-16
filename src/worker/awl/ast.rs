//! The syntax tree. Every node carries the span it came from, so the type
//! checker and the evaluator can point at the exact source.

use super::diag::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub items: Vec<Item>,
    /// Where each item starts and ends in the source (byte offsets).
    pub extents: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Let(Let),
    Fn(FnDecl),
    Worker(WorkerDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Let {
    pub name: Ident,
    pub mutable: bool,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub name: Ident,
    pub params: Vec<(Ident, TypeExpr)>,
    pub ret: Option<TypeExpr>,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerDecl {
    pub name: Ident,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Member {
    /// `timeout = 30s`
    Field { name: Ident, value: Expr },
    /// `when:`, `on_success:`, `on_failure:` followed by a block.
    Hook { name: Ident, body: Block },
}

pub type Block = Vec<Stmt>;

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let(Let),
    Assign { target: Ident, value: Expr },
    Expr(Expr),
    If { branches: Vec<(Expr, Block)>, otherwise: Option<Block>, span: Span },
    For { var: Ident, iter: Expr, body: Block, span: Span },
    Return { value: Option<Expr>, span: Span },
    Pass,
    Break(Span),
    Continue(Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeExpr {
    pub name: String,
    pub args: Vec<TypeExpr>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FPartE {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    In,
    NotIn,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::In => "in",
            BinOp::NotIn => "not in",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    FStr(Vec<FPartE>),
    /// Milliseconds.
    Duration(i64),
    Name(String),
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `f(a, b)`, and `a.f(b)` which means the same.
    Call(Ident, Vec<Expr>),
    Field(Box<Expr>, Ident),
    Index(Box<Expr>, Box<Expr>),
}
