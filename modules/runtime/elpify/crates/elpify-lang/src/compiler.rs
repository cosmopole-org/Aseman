use std::collections::HashMap;
use std::fmt::{Display, Formatter};

pub fn transpile_js_to_masm(source: &str) -> Result<String, CompilerError> {
    let mut parser = Parser::new(source);
    let program = parser.parse_program()?;
    MasmTranspiler::new().transpile(program)
}

#[derive(Debug)]
pub enum CompilerError {
    Parse(String),
    Semantic(String),
}

impl Display for CompilerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(msg) => write!(f, "parse error: {msg}"),
            Self::Semantic(msg) => write!(f, "semantic error: {msg}"),
        }
    }
}
impl std::error::Error for CompilerError {}

#[derive(Clone, Debug)]
struct ProgramAst {
    classes: Vec<ClassDecl>,
    functions: Vec<FunctionDecl>,
    statements: Vec<Stmt>,
}

#[derive(Clone, Debug)]
struct ClassDecl {
    name: String,
    methods: Vec<FunctionDecl>,
}

#[derive(Clone, Debug)]
struct FunctionDecl {
    name: String,
    params: Vec<String>,
    body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
enum Stmt {
    Let(String, Expr),
    Assign(String, Expr),
    Expr(Expr),
    Return(Expr),
    If {
        condition: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    While {
        condition: Expr,
        body: Vec<Stmt>,
    },
    For {
        init: Option<Box<Stmt>>,
        condition: Option<Expr>,
        update: Option<Box<Stmt>>,
        body: Vec<Stmt>,
    },
    Switch {
        expr: Expr,
        cases: Vec<(u64, Vec<Stmt>)>,
        default: Vec<Stmt>,
    },
    Break,
    Continue,
}

#[derive(Clone, Debug)]
enum Expr {
    Number(u64),
    Bool(bool),
    String(String),
    Null,
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    Ident(String),
    Call(String, Vec<Expr>),
    New(String, Vec<Expr>),
    MemberCall {
        object: Box<Expr>,
        method: String,
        args: Vec<Expr>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Ternary {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
}

#[derive(Clone, Copy, Debug)]
enum UnaryOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Let,
    Const,
    Class,
    New,
    Function,
    Return,
    If,
    Else,
    While,
    For,
    Switch,
    Case,
    Default,
    Break,
    Continue,
    True,
    False,
    Null,
    Ident(String),
    Number(u64),
    String(String),
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Semicolon,
    Comma,
    Dot,
    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    AndAnd,
    OrOr,
    Bang,
    Question,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(source: &str) -> Self {
        Self {
            tokens: lex(source),
            pos: 0,
        }
    }

    fn parse_program(&mut self) -> Result<ProgramAst, CompilerError> {
        let mut classes = Vec::new();
        let mut functions = Vec::new();
        let mut statements = Vec::new();

        while self.peek().is_some() {
            if self.peek() == Some(&Token::Class) {
                classes.push(self.parse_class_decl()?);
            } else if self.peek() == Some(&Token::Function) {
                functions.push(self.parse_function_decl()?);
            } else {
                statements.push(self.parse_stmt()?);
            }
        }

        Ok(ProgramAst {
            classes,
            functions,
            statements,
        })
    }

    fn parse_class_decl(&mut self) -> Result<ClassDecl, CompilerError> {
        self.expect(Token::Class)?;
        let name = self.expect_ident()?;
        self.expect(Token::LBrace)?;
        let mut methods = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            let method_name = self.expect_ident()?;
            self.expect(Token::LParen)?;
            let mut params = Vec::new();
            if self.peek() != Some(&Token::RParen) {
                loop {
                    params.push(self.expect_ident()?);
                    if self.peek() == Some(&Token::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
            }
            self.expect(Token::RParen)?;
            let body = self.parse_block()?;
            methods.push(FunctionDecl {
                name: method_name,
                params,
                body,
            });
        }
        self.expect(Token::RBrace)?;
        Ok(ClassDecl { name, methods })
    }

    fn parse_function_decl(&mut self) -> Result<FunctionDecl, CompilerError> {
        self.expect(Token::Function)?;
        let name = self.expect_ident()?;
        self.expect(Token::LParen)?;
        let mut params = Vec::new();
        if self.peek() != Some(&Token::RParen) {
            loop {
                params.push(self.expect_ident()?);
                if self.peek() == Some(&Token::Comma) {
                    self.bump();
                } else {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        let body = self.parse_block()?;
        Ok(FunctionDecl { name, params, body })
    }

    fn parse_block(&mut self) -> Result<Vec<Stmt>, CompilerError> {
        self.expect(Token::LBrace)?;
        let mut stmts = Vec::new();
        while self.peek() != Some(&Token::RBrace) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(Token::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Stmt, CompilerError> {
        match self.peek() {
            Some(Token::Let) | Some(Token::Const) => {
                self.bump();
                let name = self.expect_ident()?;
                self.expect(Token::Assign)?;
                let expr = self.parse_expr()?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Let(name, expr))
            }
            Some(Token::Return) => {
                self.bump();
                let expr = self.parse_expr()?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Return(expr))
            }
            Some(Token::If) => {
                self.bump();
                self.expect(Token::LParen)?;
                let condition = self.parse_expr()?;
                self.expect(Token::RParen)?;
                let then_body = self.parse_block()?;
                let else_body = if self.peek() == Some(&Token::Else) {
                    self.bump();
                    self.parse_block()?
                } else {
                    vec![]
                };
                Ok(Stmt::If {
                    condition,
                    then_body,
                    else_body,
                })
            }
            Some(Token::While) => {
                self.bump();
                self.expect(Token::LParen)?;
                let condition = self.parse_expr()?;
                self.expect(Token::RParen)?;
                let body = self.parse_block()?;
                Ok(Stmt::While { condition, body })
            }
            Some(Token::For) => self.parse_for(),
            Some(Token::Switch) => self.parse_switch(),
            Some(Token::Break) => {
                self.bump();
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Break)
            }
            Some(Token::Continue) => {
                self.bump();
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Continue)
            }
            Some(Token::Ident(_)) => {
                if self.peek_next() == Some(&Token::Assign) {
                    let name = self.expect_ident()?;
                    self.expect(Token::Assign)?;
                    let expr = self.parse_expr()?;
                    self.expect(Token::Semicolon)?;
                    Ok(Stmt::Assign(name, expr))
                } else {
                    let expr = self.parse_expr()?;
                    self.expect(Token::Semicolon)?;
                    Ok(Stmt::Expr(expr))
                }
            }
            _ => {
                let expr = self.parse_expr()?;
                self.expect(Token::Semicolon)?;
                Ok(Stmt::Expr(expr))
            }
        }
    }

    fn parse_switch(&mut self) -> Result<Stmt, CompilerError> {
        self.expect(Token::Switch)?;
        self.expect(Token::LParen)?;
        let expr = self.parse_expr()?;
        self.expect(Token::RParen)?;
        self.expect(Token::LBrace)?;

        let mut cases = Vec::new();
        let mut default = Vec::new();

        while self.peek() != Some(&Token::RBrace) {
            match self.peek() {
                Some(Token::Case) => {
                    self.bump();
                    let value = self.expect_number()?;
                    self.expect(Token::Colon)?;
                    let mut body = Vec::new();
                    while self.peek() != Some(&Token::Case)
                        && self.peek() != Some(&Token::Default)
                        && self.peek() != Some(&Token::RBrace)
                    {
                        if self.peek() == Some(&Token::Break) {
                            self.bump();
                            self.expect(Token::Semicolon)?;
                            break;
                        }
                        body.push(self.parse_stmt()?);
                    }
                    cases.push((value, body));
                }
                Some(Token::Default) => {
                    self.bump();
                    self.expect(Token::Colon)?;
                    while self.peek() != Some(&Token::RBrace) {
                        if self.peek() == Some(&Token::Break) {
                            self.bump();
                            self.expect(Token::Semicolon)?;
                            break;
                        }
                        default.push(self.parse_stmt()?);
                    }
                }
                other => {
                    return Err(CompilerError::Parse(format!(
                        "unexpected token in switch: {other:?}"
                    )));
                }
            }
        }

        self.expect(Token::RBrace)?;
        Ok(Stmt::Switch {
            expr,
            cases,
            default,
        })
    }

    fn parse_for(&mut self) -> Result<Stmt, CompilerError> {
        self.expect(Token::For)?;
        self.expect(Token::LParen)?;

        let init = if self.peek() == Some(&Token::Semicolon) {
            self.bump();
            None
        } else if self.peek() == Some(&Token::Let) || self.peek() == Some(&Token::Const) {
            self.bump();
            let name = self.expect_ident()?;
            self.expect(Token::Assign)?;
            let expr = self.parse_expr()?;
            self.expect(Token::Semicolon)?;
            Some(Box::new(Stmt::Let(name, expr)))
        } else {
            let expr_or_assign = self.parse_for_update_stmt()?;
            self.expect(Token::Semicolon)?;
            Some(Box::new(expr_or_assign))
        };

        let condition = if self.peek() == Some(&Token::Semicolon) {
            self.bump();
            None
        } else {
            let c = self.parse_expr()?;
            self.expect(Token::Semicolon)?;
            Some(c)
        };

        let update = if self.peek() == Some(&Token::RParen) {
            None
        } else {
            Some(Box::new(self.parse_for_update_stmt()?))
        };
        self.expect(Token::RParen)?;
        let body = self.parse_block()?;
        Ok(Stmt::For {
            init,
            condition,
            update,
            body,
        })
    }

    fn parse_for_update_stmt(&mut self) -> Result<Stmt, CompilerError> {
        if matches!(self.peek(), Some(Token::Ident(_))) && self.peek_next() == Some(&Token::Assign)
        {
            let name = self.expect_ident()?;
            self.expect(Token::Assign)?;
            let expr = self.parse_expr()?;
            Ok(Stmt::Assign(name, expr))
        } else {
            Ok(Stmt::Expr(self.parse_expr()?))
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, CompilerError> {
        self.parse_ternary()
    }

    fn parse_ternary(&mut self) -> Result<Expr, CompilerError> {
        let condition = self.parse_or()?;
        if self.peek() == Some(&Token::Question) {
            self.bump();
            let then_expr = self.parse_expr()?;
            self.expect(Token::Colon)?;
            let else_expr = self.parse_expr()?;
            Ok(Expr::Ternary {
                condition: Box::new(condition),
                then_expr: Box::new(then_expr),
                else_expr: Box::new(else_expr),
            })
        } else {
            Ok(condition)
        }
    }
    fn parse_or(&mut self) -> Result<Expr, CompilerError> {
        let mut left = self.parse_and()?;
        while self.peek() == Some(&Token::OrOr) {
            self.bump();
            let right = self.parse_and()?;
            left = Expr::Binary(Box::new(left), BinOp::Or, Box::new(right));
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Expr, CompilerError> {
        let mut left = self.parse_eq()?;
        while self.peek() == Some(&Token::AndAnd) {
            self.bump();
            let right = self.parse_eq()?;
            left = Expr::Binary(Box::new(left), BinOp::And, Box::new(right));
        }
        Ok(left)
    }
    fn parse_eq(&mut self) -> Result<Expr, CompilerError> {
        let mut left = self.parse_cmp()?;
        loop {
            match self.peek() {
                Some(Token::EqEq) => {
                    self.bump();
                    let right = self.parse_cmp()?;
                    left = Expr::Binary(Box::new(left), BinOp::Eq, Box::new(right));
                }
                Some(Token::NotEq) => {
                    self.bump();
                    let right = self.parse_cmp()?;
                    left = Expr::Binary(Box::new(left), BinOp::Neq, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }
    fn parse_cmp(&mut self) -> Result<Expr, CompilerError> {
        let mut left = self.parse_add_sub()?;
        loop {
            match self.peek() {
                Some(Token::Lt) => {
                    self.bump();
                    let right = self.parse_add_sub()?;
                    left = Expr::Binary(Box::new(left), BinOp::Lt, Box::new(right));
                }
                Some(Token::Lte) => {
                    self.bump();
                    let right = self.parse_add_sub()?;
                    left = Expr::Binary(Box::new(left), BinOp::Lte, Box::new(right));
                }
                Some(Token::Gt) => {
                    self.bump();
                    let right = self.parse_add_sub()?;
                    left = Expr::Binary(Box::new(left), BinOp::Gt, Box::new(right));
                }
                Some(Token::Gte) => {
                    self.bump();
                    let right = self.parse_add_sub()?;
                    left = Expr::Binary(Box::new(left), BinOp::Gte, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }
    fn parse_add_sub(&mut self) -> Result<Expr, CompilerError> {
        let mut left = self.parse_mul_div()?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.bump();
                    let right = self.parse_mul_div()?;
                    left = Expr::Binary(Box::new(left), BinOp::Add, Box::new(right));
                }
                Some(Token::Minus) => {
                    self.bump();
                    let right = self.parse_mul_div()?;
                    left = Expr::Binary(Box::new(left), BinOp::Sub, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }
    fn parse_mul_div(&mut self) -> Result<Expr, CompilerError> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Token::Star) => {
                    self.bump();
                    let right = self.parse_unary()?;
                    left = Expr::Binary(Box::new(left), BinOp::Mul, Box::new(right));
                }
                Some(Token::Slash) => {
                    self.bump();
                    let right = self.parse_unary()?;
                    left = Expr::Binary(Box::new(left), BinOp::Div, Box::new(right));
                }
                Some(Token::Percent) => {
                    self.bump();
                    let right = self.parse_unary()?;
                    left = Expr::Binary(Box::new(left), BinOp::Mod, Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr, CompilerError> {
        match self.peek() {
            Some(Token::Bang) => {
                self.bump();
                Ok(Expr::Unary(UnaryOp::Not, Box::new(self.parse_unary()?)))
            }
            Some(Token::Minus) => {
                self.bump();
                Ok(Expr::Unary(UnaryOp::Neg, Box::new(self.parse_unary()?)))
            }
            _ => self.parse_factor(),
        }
    }

    fn parse_factor(&mut self) -> Result<Expr, CompilerError> {
        match self.bump() {
            Some(Token::New) => {
                let class_name = self.expect_ident()?;
                self.expect(Token::LParen)?;
                let mut args = Vec::new();
                if self.peek() != Some(&Token::RParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if self.peek() == Some(&Token::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RParen)?;
                Ok(Expr::New(class_name, args))
            }
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::True) => Ok(Expr::Bool(true)),
            Some(Token::False) => Ok(Expr::Bool(false)),
            Some(Token::Null) => Ok(Expr::Null),
            Some(Token::String(s)) => Ok(Expr::String(s)),
            Some(Token::LBracket) => {
                let mut items = Vec::new();
                if self.peek() != Some(&Token::RBracket) {
                    loop {
                        items.push(self.parse_expr()?);
                        if self.peek() == Some(&Token::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RBracket)?;
                Ok(Expr::Array(items))
            }
            Some(Token::LBrace) => {
                let mut fields = Vec::new();
                if self.peek() != Some(&Token::RBrace) {
                    loop {
                        let key = match self.bump() {
                            Some(Token::Ident(k)) => k,
                            Some(Token::String(k)) => k,
                            other => {
                                return Err(CompilerError::Parse(format!(
                                    "expected object key, got {other:?}"
                                )));
                            }
                        };
                        self.expect(Token::Colon)?;
                        let value = self.parse_expr()?;
                        fields.push((key, value));
                        if self.peek() == Some(&Token::Comma) {
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                self.expect(Token::RBrace)?;
                Ok(Expr::Object(fields))
            }
            Some(Token::Ident(name)) => {
                if self.peek() == Some(&Token::Dot) {
                    self.bump();
                    let method = self.expect_ident()?;
                    self.expect(Token::LParen)?;
                    let mut args = Vec::new();
                    if self.peek() != Some(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.peek() == Some(&Token::Comma) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(Token::RParen)?;
                    Ok(Expr::MemberCall {
                        object: Box::new(Expr::Ident(name)),
                        method,
                        args,
                    })
                } else if self.peek() == Some(&Token::LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    if self.peek() != Some(&Token::RParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if self.peek() == Some(&Token::Comma) {
                                self.bump();
                            } else {
                                break;
                            }
                        }
                    }
                    self.expect(Token::RParen)?;
                    Ok(Expr::Call(name, args))
                } else {
                    Ok(Expr::Ident(name))
                }
            }
            Some(Token::LParen) => {
                let expr = self.parse_expr()?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            other => Err(CompilerError::Parse(format!(
                "unexpected token in factor: {other:?}"
            ))),
        }
    }

    fn expect_ident(&mut self) -> Result<String, CompilerError> {
        match self.bump() {
            Some(Token::Ident(v)) => Ok(v),
            other => Err(CompilerError::Parse(format!(
                "expected identifier, got {other:?}"
            ))),
        }
    }
    fn expect_number(&mut self) -> Result<u64, CompilerError> {
        match self.bump() {
            Some(Token::Number(v)) => Ok(v),
            other => Err(CompilerError::Parse(format!(
                "expected number, got {other:?}"
            ))),
        }
    }
    fn expect(&mut self, token: Token) -> Result<(), CompilerError> {
        let got = self.bump();
        if got == Some(token.clone()) {
            Ok(())
        } else {
            Err(CompilerError::Parse(format!(
                "expected {token:?}, got {got:?}"
            )))
        }
    }
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }
    fn peek_next(&self) -> Option<&Token> {
        self.tokens.get(self.pos + 1)
    }
    fn bump(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
}

#[derive(Default)]
struct FuncCtx {
    slots: HashMap<String, usize>,
    var_classes: HashMap<String, String>,
    next_slot: usize,
}
impl FuncCtx {
    fn define(&mut self, name: &str) -> usize {
        if let Some(idx) = self.slots.get(name) {
            *idx
        } else {
            let idx = self.next_slot;
            self.next_slot += 1;
            self.slots.insert(name.to_string(), idx);
            idx
        }
    }
    fn get(&self, name: &str) -> Option<usize> {
        self.slots.get(name).copied()
    }
    fn set_class(&mut self, var: &str, class_name: &str) {
        self.var_classes
            .insert(var.to_string(), class_name.to_string());
    }
    fn get_class(&self, var: &str) -> Option<&str> {
        self.var_classes.get(var).map(String::as_str)
    }
}

struct MasmTranspiler {
    functions: HashMap<String, FunctionDecl>,
    classes: HashMap<String, ClassDecl>,
}
impl MasmTranspiler {
    fn new() -> Self {
        Self {
            functions: HashMap::new(),
            classes: HashMap::new(),
        }
    }

    fn transpile(&mut self, program: ProgramAst) -> Result<String, CompilerError> {
        for c in &program.classes {
            self.classes.insert(c.name.clone(), c.clone());
        }
        for f in &program.functions {
            self.functions.insert(f.name.clone(), f.clone());
        }

        let mut out = String::new();

        for class in &program.classes {
            for method in &class.methods {
                let mut method_fn = method.clone();
                method_fn.name = format!("{}__{}", class.name, method_fn.name);
                method_fn.params.insert(0, "this".to_string());
                let proc = self.transpile_function(&method_fn)?;
                out.push_str(&proc);
                out.push('\n');
            }
        }

        // user functions first
        for func in &program.functions {
            let proc = self.transpile_function(func)?;
            out.push_str(&proc);
            out.push('\n');
        }

        // synthetic main proc
        let main_fn = FunctionDecl {
            name: "main".to_string(),
            params: vec![],
            body: program.statements,
        };
        out.push_str(&self.transpile_function(&main_fn)?);
        out.push('\n');

        out.push_str("begin\n    exec.main\nend\n");
        Ok(out)
    }

    fn transpile_function(&self, func: &FunctionDecl) -> Result<String, CompilerError> {
        let mut ctx = FuncCtx::default();
        for p in &func.params {
            ctx.define(p);
        }

        let mut body_lines = Vec::new();

        // store parameters from stack into local slots
        for p in func.params.iter().rev() {
            let slot = ctx
                .get(p)
                .ok_or_else(|| CompilerError::Semantic("missing param slot".to_string()))?;
            body_lines.push(format!("loc_store.{slot}"));
        }

        let has_return = self.emit_statements(&func.body, &mut ctx, &mut body_lines)?;

        if !has_return {
            // default return 0
            body_lines.push("push.0".to_string());
            body_lines.push("swap.1".to_string());
            body_lines.push("drop".to_string());
        }

        let mut out = format!("proc.{}.{}\n", func.name, ctx.next_slot.max(1));
        for l in body_lines {
            out.push_str("    ");
            out.push_str(&l);
            out.push('\n');
        }
        out.push_str("end\n");
        Ok(out)
    }

    fn emit_statements(
        &self,
        stmts: &[Stmt],
        ctx: &mut FuncCtx,
        out: &mut Vec<String>,
    ) -> Result<bool, CompilerError> {
        let mut returned = false;
        for stmt in stmts {
            if returned {
                break;
            }
            match stmt {
                Stmt::Let(name, expr) => {
                    self.emit_expr(expr, ctx, out)?;
                    let slot = ctx.define(name);
                    out.push(format!("loc_store.{slot}"));
                    if let Expr::New(class_name, _) = expr {
                        ctx.set_class(name, class_name);
                    }
                }
                Stmt::Assign(name, expr) => {
                    self.emit_expr(expr, ctx, out)?;
                    let slot = ctx.get(name).ok_or_else(|| {
                        CompilerError::Semantic(format!("unknown variable '{name}'"))
                    })?;
                    out.push(format!("loc_store.{slot}"));
                    if let Expr::New(class_name, _) = expr {
                        ctx.set_class(name, class_name);
                    }
                }
                Stmt::Expr(expr) => {
                    self.emit_expr(expr, ctx, out)?;
                    out.push("drop".to_string());
                }
                Stmt::Return(expr) => {
                    self.emit_expr(expr, ctx, out)?;
                    out.push("swap.1".to_string());
                    out.push("drop".to_string());
                    returned = true;
                }
                Stmt::If {
                    condition,
                    then_body,
                    else_body,
                } => {
                    self.emit_expr(condition, ctx, out)?;
                    out.push("if.true".to_string());
                    let then_returns = self.emit_statements(then_body, ctx, out)?;
                    if !else_body.is_empty() {
                        out.push("else".to_string());
                        let else_returns = self.emit_statements(else_body, ctx, out)?;
                        returned = then_returns && else_returns;
                    }
                    out.push("end".to_string());
                }
                Stmt::While { condition, body } => {
                    self.emit_expr(condition, ctx, out)?;
                    out.push("while.true".to_string());
                    let _ = self.emit_statements(body, ctx, out)?;
                    self.emit_expr(condition, ctx, out)?;
                    out.push("end".to_string());
                }
                Stmt::For {
                    init,
                    condition,
                    update,
                    body,
                } => {
                    if let Some(init_stmt) = init {
                        let _ = self.emit_statements(&[*init_stmt.clone()], ctx, out)?;
                    }
                    if let Some(cond) = condition {
                        self.emit_expr(cond, ctx, out)?;
                    } else {
                        out.push("push.1".to_string());
                    }
                    out.push("while.true".to_string());
                    let _ = self.emit_statements(body, ctx, out)?;
                    if let Some(update_stmt) = update {
                        let _ = self.emit_statements(&[*update_stmt.clone()], ctx, out)?;
                    }
                    if let Some(cond) = condition {
                        self.emit_expr(cond, ctx, out)?;
                    } else {
                        out.push("push.1".to_string());
                    }
                    out.push("end".to_string());
                }
                Stmt::Switch {
                    expr,
                    cases,
                    default,
                } => {
                    self.emit_expr(expr, ctx, out)?;
                    let switch_slot = ctx.define("__switch_tmp");
                    out.push(format!("loc_store.{switch_slot}"));

                    self.emit_switch_chain(0, cases, default, switch_slot, ctx, out)?;
                }
                Stmt::Break | Stmt::Continue => {}
            }
        }
        Ok(returned)
    }

    fn emit_switch_chain(
        &self,
        idx: usize,
        cases: &[(u64, Vec<Stmt>)],
        default: &[Stmt],
        switch_slot: usize,
        ctx: &mut FuncCtx,
        out: &mut Vec<String>,
    ) -> Result<(), CompilerError> {
        if idx >= cases.len() {
            self.emit_statements(default, ctx, out)?;
            return Ok(());
        }
        let (case_value, case_body) = &cases[idx];
        out.push(format!("loc_load.{switch_slot}"));
        out.push(format!("push.{case_value}"));
        out.push("eq".to_string());
        out.push("if.true".to_string());
        let _ = self.emit_statements(case_body, ctx, out)?;
        out.push("else".to_string());
        self.emit_switch_chain(idx + 1, cases, default, switch_slot, ctx, out)?;
        out.push("end".to_string());
        Ok(())
    }

    fn emit_expr(
        &self,
        expr: &Expr,
        ctx: &FuncCtx,
        out: &mut Vec<String>,
    ) -> Result<(), CompilerError> {
        match expr {
            Expr::Number(n) => out.push(format!("push.{n}")),
            Expr::Bool(b) => out.push(format!("push.{}", if *b { 1 } else { 0 })),
            Expr::String(s) => out.push(format!("push.{}", hash_str(s))),
            Expr::Null => out.push("push.0".to_string()),
            Expr::Array(items) => {
                let encoded = encode_collection_hash("arr", items, ctx)?;
                out.push(format!("push.{encoded}"));
            }
            Expr::Object(fields) => {
                let encoded = encode_object_hash(fields, ctx)?;
                out.push(format!("push.{encoded}"));
            }
            Expr::Ident(name) => {
                let slot = ctx
                    .get(name)
                    .ok_or_else(|| CompilerError::Semantic(format!("unknown variable '{name}'")))?;
                out.push(format!("loc_load.{slot}"));
            }
            Expr::Call(name, args) => {
                for arg in args {
                    self.emit_expr(arg, ctx, out)?;
                }
                out.push(format!("exec.{name}"));
            }
            Expr::New(class_name, args) => {
                for arg in args {
                    self.emit_expr(arg, ctx, out)?;
                }
                let ctor_name = format!("{class_name}__constructor");
                if self.functions.contains_key(&ctor_name)
                    || self
                        .classes
                        .get(class_name)
                        .map(|c| c.methods.iter().any(|m| m.name == "constructor"))
                        .unwrap_or(false)
                {
                    out.push(format!("exec.{ctor_name}"));
                } else {
                    out.push("push.0".to_string());
                }
            }
            Expr::MemberCall {
                object,
                method,
                args,
            } => {
                let class_name = if let Expr::Ident(var) = object.as_ref() {
                    ctx.get_class(var).map(str::to_string)
                } else {
                    None
                }
                .ok_or_else(|| {
                    CompilerError::Semantic(
                        "member calls require an instance variable created with `new`".to_string(),
                    )
                })?;

                self.emit_expr(object, ctx, out)?;
                for arg in args {
                    self.emit_expr(arg, ctx, out)?;
                }
                out.push(format!("exec.{}__{}", class_name, method));
            }
            Expr::Unary(op, inner) => {
                self.emit_expr(inner, ctx, out)?;
                match op {
                    UnaryOp::Not => out.push("not".to_string()),
                    UnaryOp::Neg => out.push("neg".to_string()),
                }
            }
            Expr::Binary(left, op, right) => {
                self.emit_expr(left, ctx, out)?;
                self.emit_expr(right, ctx, out)?;
                match op {
                    BinOp::Add => out.push("add".to_string()),
                    BinOp::Sub => out.push("sub".to_string()),
                    BinOp::Mul => out.push("mul".to_string()),
                    BinOp::Div => out.push("div".to_string()),
                    BinOp::Mod => out.push("mod".to_string()),
                    BinOp::Eq => out.push("eq".to_string()),
                    BinOp::Neq => {
                        out.push("eq".to_string());
                        out.push("not".to_string());
                    }
                    BinOp::Lt => out.push("lt".to_string()),
                    BinOp::Lte => {
                        out.push("gt".to_string());
                        out.push("not".to_string());
                    }
                    BinOp::Gt => out.push("gt".to_string()),
                    BinOp::Gte => {
                        out.push("lt".to_string());
                        out.push("not".to_string());
                    }
                    BinOp::And => out.push("and".to_string()),
                    BinOp::Or => out.push("or".to_string()),
                }
            }
            Expr::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.emit_expr(condition, ctx, out)?;
                out.push("if.true".to_string());
                self.emit_expr(then_expr, ctx, out)?;
                out.push("else".to_string());
                self.emit_expr(else_expr, ctx, out)?;
                out.push("end".to_string());
            }
        }
        Ok(())
    }
}

fn hash_str(input: &str) -> u64 {
    // FNV-1a 64-bit
    let mut hash = 0xcbf29ce484222325u64;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn encode_const_expr(expr: &Expr, ctx: &FuncCtx) -> Result<u64, CompilerError> {
    match expr {
        Expr::Number(n) => Ok(*n),
        Expr::Bool(b) => Ok(if *b { 1 } else { 0 }),
        Expr::String(s) => Ok(hash_str(s)),
        Expr::Null => Ok(0),
        Expr::Array(items) => encode_collection_hash("arr", items, ctx),
        Expr::Object(fields) => encode_object_hash(fields, ctx),
        Expr::Ident(name) => ctx
            .get(name)
            .map(|idx| 1_000_000 + idx as u64)
            .ok_or_else(|| CompilerError::Semantic(format!("unknown variable '{name}'"))),
        Expr::Unary(UnaryOp::Not, inner) => Ok(u64::from(encode_const_expr(inner, ctx)? == 0)),
        Expr::Unary(UnaryOp::Neg, inner) => Ok(0u64.wrapping_sub(encode_const_expr(inner, ctx)?)),
        Expr::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            if encode_const_expr(condition, ctx)? != 0 {
                encode_const_expr(then_expr, ctx)
            } else {
                encode_const_expr(else_expr, ctx)
            }
        }
        _ => Err(CompilerError::Semantic(
            "array/object literals currently require literal-like elements".to_string(),
        )),
    }
}

fn encode_collection_hash(
    prefix: &str,
    items: &[Expr],
    ctx: &FuncCtx,
) -> Result<u64, CompilerError> {
    let mut acc = hash_str(prefix);
    for item in items {
        let v = encode_const_expr(item, ctx)?;
        acc ^= v;
        acc = acc.wrapping_mul(0x100000001b3);
    }
    Ok(acc)
}

fn encode_object_hash(fields: &[(String, Expr)], ctx: &FuncCtx) -> Result<u64, CompilerError> {
    let mut acc = hash_str("obj");
    for (k, vexpr) in fields {
        acc ^= hash_str(k);
        acc = acc.wrapping_mul(0x100000001b3);
        acc ^= encode_const_expr(vexpr, ctx)?;
        acc = acc.wrapping_mul(0x100000001b3);
    }
    Ok(acc)
}

fn lex(source: &str) -> Vec<Token> {
    let mut chars = source.chars().peekable();
    let mut tokens = Vec::new();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        if ch.is_ascii_digit() {
            let mut n = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    n.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Number(n.parse().unwrap()));
            continue;
        }

        if ch == '"' {
            chars.next();
            let mut s = String::new();
            while let Some(&c) = chars.peek() {
                chars.next();
                if c == '"' {
                    break;
                }
                s.push(c);
            }
            tokens.push(Token::String(s));
            continue;
        }

        if ch.is_ascii_alphabetic() || ch == '_' {
            let mut ident = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    ident.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(match ident.as_str() {
                "let" => Token::Let,
                "const" => Token::Const,
                "class" => Token::Class,
                "new" => Token::New,
                "function" => Token::Function,
                "return" => Token::Return,
                "if" => Token::If,
                "else" => Token::Else,
                "while" => Token::While,
                "for" => Token::For,
                "switch" => Token::Switch,
                "case" => Token::Case,
                "default" => Token::Default,
                "break" => Token::Break,
                "continue" => Token::Continue,
                "true" => Token::True,
                "false" => Token::False,
                "null" => Token::Null,
                _ => Token::Ident(ident),
            });
            continue;
        }

        let two = {
            let mut it = chars.clone();
            let a = it.next();
            let b = it.next();
            a.zip(b).map(|(x, y)| format!("{x}{y}"))
        };
        if let Some(op) = two {
            let tok = match op.as_str() {
                "==" => Some(Token::EqEq),
                "!=" => Some(Token::NotEq),
                "<=" => Some(Token::Lte),
                ">=" => Some(Token::Gte),
                "&&" => Some(Token::AndAnd),
                "||" => Some(Token::OrOr),
                _ => None,
            };
            if let Some(t) = tok {
                chars.next();
                chars.next();
                tokens.push(t);
                continue;
            }
        }

        let tok = match ch {
            '(' => Token::LParen,
            ')' => Token::RParen,
            '{' => Token::LBrace,
            '}' => Token::RBrace,
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            ':' => Token::Colon,
            ';' => Token::Semicolon,
            ',' => Token::Comma,
            '.' => Token::Dot,
            '=' => Token::Assign,
            '+' => Token::Plus,
            '-' => Token::Minus,
            '*' => Token::Star,
            '/' => Token::Slash,
            '%' => Token::Percent,
            '!' => Token::Bang,
            '?' => Token::Question,
            '<' => Token::Lt,
            '>' => Token::Gt,
            _ => {
                chars.next();
                continue;
            }
        };
        chars.next();
        tokens.push(tok);
    }

    tokens
}
