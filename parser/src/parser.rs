use std::{ops::Range, path::PathBuf};

use chumsky::{prelude::*, Stream};

use crate::{
    ast::{
        Assignment, BinOp, BinOpKind, Call, ElsePart, Expr, ExprKind, File, FnDecl, IfStmt, Item,
        Literal, NameTyPair, Stmt, StructDecl, Ty, TyKind, UnaryOp, UnaryOpKind, VarDecl,
        WhileStmt,
    },
    lexer::Token,
};

type Error<'src> = Simple<Token<'src>>;
type Span = Range<usize>;

fn ident_parser<'src>() -> impl Parser<Token<'src>, String, Error = Error<'src>> + Clone {
    let ident = select! {
        Token::Ident(ident) => ident.to_owned(),
    };
    ident.labelled("identifier").boxed()
}

fn ty_parser<'src>() -> impl Parser<Token<'src>, Ty, Error = Error<'src>> + Clone {
    recursive(|ty_parser| {
        let primitive = filter_map(|span, token| {
            let kind = match token {
                Token::Ident("u64") => TyKind::U64,
                _ => return Err(Simple::expected_input_found(span, Vec::new(), Some(token))),
            };
            Ok(Ty { span, kind })
        })
        .labelled("primitive type");

        let ptr = just(Token::Ptr)
            .ignore_then(ty_parser.clone())
            .map_with_span(|ty: Ty, span| Ty {
                kind: TyKind::Ptr(Box::new(ty)),
                span,
            })
            .labelled("pointer type");

        let name = ident_parser()
            .map_with_span(|name: String, span| Ty {
                kind: TyKind::Name(name),
                span,
            })
            .labelled("name type");

        primitive.or(ptr).or(name).labelled("type").boxed()
    })
}

fn expr_parser<'src>() -> impl Parser<Token<'src>, Expr, Error = Error<'src>> + Clone {
    recursive(|expr| {
        let literal = filter_map(|span: Span, token| match token {
            Token::String(str) => Ok(Expr {
                kind: ExprKind::Literal(Literal::String(
                    str[1..str.len() - 2].to_owned(),
                    span.clone(),
                )),
                span,
            }),
            // todo lol unwrap
            Token::Integer(int) => Ok(Expr {
                kind: ExprKind::Literal(Literal::Integer(int.parse().unwrap(), span.clone())),
                span,
            }),
            _ => Err(Simple::expected_input_found(span, Vec::new(), Some(token))),
        })
        .labelled("literal");

        let expr_list = expr
            .clone()
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .or_not()
            .map(|item| item.unwrap_or_default())
            .boxed();

        let array = expr_list
            .clone()
            .delimited_by(just(Token::BracketO), just(Token::BracketC))
            .map_with_span(|exprs: Vec<Expr>, span| Expr {
                kind: ExprKind::Array(exprs),
                span,
            });

        let atom = literal
            .or(ident_parser().map_with_span(|name, span| Expr {
                kind: ExprKind::Name(name),
                span,
            }))
            .or(array)
            .or(expr
                .clone()
                .delimited_by(just(Token::ParenO), just(Token::ParenC)))
            .boxed();

        let call = atom
            .clone()
            .then(
                expr_list
                    .delimited_by(just(Token::ParenO), just(Token::ParenC))
                    .repeated(),
            )
            .foldl(|callee: Expr, args: Vec<Expr>| {
                let span =
                    callee.span.start..args.last().map(|e| e.span.end).unwrap_or(callee.span.end);
                Expr {
                    kind: ExprKind::Call(Call {
                        callee: Box::new(callee),
                        args,
                    }),
                    span,
                }
            })
            .labelled("call")
            .boxed();

        let unary_op = choice((
            just(Token::Minus).to(UnaryOpKind::Neg),
            just(Token::Bang).to(UnaryOpKind::Not),
            just(Token::Ampersand).to(UnaryOpKind::AddrOf),
            just(Token::Asterisk).to(UnaryOpKind::Deref),
        ))
        .repeated()
        .then(call)
        .foldr(|kind, rhs| {
            let span = rhs.span.clone();
            Expr {
                kind: ExprKind::UnaryOp(UnaryOp {
                    expr: Box::new(rhs),
                    kind,
                    span: span.clone(),
                }),
                span,
            }
        })
        .labelled("unary")
        .boxed();

        let op = just(Token::Asterisk)
            .to(BinOpKind::Mul)
            .or(just(Token::Slash).to(BinOpKind::Div));

        let product = unary_op
            .clone()
            .then(op.then(unary_op).repeated())
            .foldl(|a, (kind, b)| {
                let span = a.span.start..b.span.end;
                Expr {
                    kind: ExprKind::BinOp(BinOp {
                        kind,
                        lhs: Box::new(a),
                        rhs: Box::new(b),
                        span: span.clone(),
                    }),
                    span,
                }
            });

        // Sum ops (add and subtract) have equal precedence
        let op = just(Token::Plus)
            .to(BinOpKind::Add)
            .or(just(Token::Minus).to(BinOpKind::Sub));
        let sum = product
            .clone()
            .then(op.then(product).repeated())
            .foldl(|a, (kind, b)| {
                let span = a.span.start..b.span.end;
                Expr {
                    kind: ExprKind::BinOp(BinOp {
                        kind,
                        lhs: Box::new(a),
                        rhs: Box::new(b),
                        span: span.clone(),
                    }),
                    span,
                }
            })
            .labelled("product")
            .boxed();

        // Comparison ops (equal, not-equal) have equal precedence
        let op = just(Token::EqEq)
            .to(BinOpKind::Eq)
            .or(just(Token::BangEq).to(BinOpKind::Neq));
        let compare = sum
            .clone()
            .then(op.then(sum).repeated())
            .foldl(|a, (kind, b)| {
                let span = a.span.start..b.span.end;
                Expr {
                    kind: ExprKind::BinOp(BinOp {
                        kind,
                        lhs: Box::new(a),
                        rhs: Box::new(b),
                        span: span.clone(),
                    }),
                    span,
                }
            });
        compare.labelled("comparison").boxed()
    })
}

fn statement_parser<'src>() -> impl Parser<Token<'src>, Stmt, Error = Error<'src>> + Clone {
    recursive(|stmt| {
        let var_decl = ty_parser()
            .then(ident_parser())
            .then_ignore(just(Token::Eq))
            .then(expr_parser())
            .then_ignore(just(Token::Semi))
            .map(|((ty, name), rhs)| {
                Stmt::VarDecl(VarDecl {
                    name,
                    ty,
                    rhs: Some(rhs),
                    span: Default::default(),
                })
            })
            .boxed();

        let assignment = expr_parser()
            .then_ignore(just(Token::Eq))
            .then(expr_parser())
            .then_ignore(just(Token::Semi))
            .map(|(place, rhs)| {
                Stmt::Assignment(Assignment {
                    place,
                    rhs,
                    span: Default::default(),
                })
            });

        let block = stmt
            .clone()
            .repeated()
            .delimited_by(just(Token::BraceO), just(Token::BraceC));

        let while_loop = just(Token::While)
            .ignore_then(expr_parser())
            .then(block.clone())
            .map_with_span(|(cond, body), span| Stmt::WhileStmt(WhileStmt { cond, body, span }))
            .labelled("while loop");

        let if_stmt = recursive(|if_stmt| {
            just(Token::If)
                .ignore_then(expr_parser())
                .then(block.clone())
                .then(
                    just(Token::Else)
                        .ignore_then(
                            if_stmt
                                .map(|if_stmt| ElsePart::ElseIf(Box::new(if_stmt)))
                                .or(block.clone().map_with_span(ElsePart::Else)),
                        )
                        .or_not(),
                )
                .map_with_span(|((cond, body), else_part), span| IfStmt {
                    cond,
                    body,
                    else_part,
                    span,
                })
        })
        .map(Stmt::IfStmt)
        .boxed();

        var_decl
            .or(assignment)
            .or(expr_parser().then_ignore(just(Token::Semi)).map(Stmt::Expr))
            .or(if_stmt)
            .or(while_loop)
    })
    .labelled("statement")
    .boxed()
}

fn name_ty_pair_parser<'src>() -> impl Parser<Token<'src>, NameTyPair, Error = Error<'src>> + Clone
{
    ident_parser()
        .then_ignore(just(Token::Colon))
        .then(ty_parser())
        .map_with_span(|(name, ty), span| NameTyPair { name, ty, span })
}

fn struct_parser<'src>() -> impl Parser<Token<'src>, StructDecl, Error = Error<'src>> + Clone {
    let name = just(Token::Struct).ignore_then(ident_parser());

    let fields = name_ty_pair_parser()
        .separated_by(just(Token::Comma))
        .delimited_by(just(Token::BraceO), just(Token::BraceC));

    name.then(fields)
        .map(|(name, fields)| StructDecl {
            name,
            fields,
            span: Default::default(),
        })
        .labelled("struct")
}

fn item_parser<'src>() -> impl Parser<Token<'src>, Item, Error = Error<'src>> + Clone {
    // ---- function

    let name = ident_parser();

    let params = name_ty_pair_parser()
        .separated_by(just(Token::Comma))
        .allow_trailing()
        .delimited_by(just(Token::ParenO), just(Token::ParenC))
        .labelled("function arguments");

    let ret_ty = just(Token::Arrow).ignore_then(ty_parser()).or_not();
    let function = just(Token::Fn)
        .ignore_then(name)
        .then(params)
        .then(ret_ty)
        .then(
            statement_parser()
                .repeated()
                .delimited_by(just(Token::BraceO), just(Token::BraceC)),
        )
        .map_with_span(|(((name, params), ret_ty), body), span| FnDecl {
            name,
            params,
            ret_ty,
            span,
            body,
        })
        .labelled("function");

    // ---- item

    function
        .map(Item::FnDecl)
        .or(struct_parser().map(Item::StructDecl))
        .labelled("item")
}

fn file_parser<'src>(
    file_name: PathBuf,
) -> impl Parser<Token<'src>, File, Error = Error<'src>> + Clone {
    item_parser()
        .repeated()
        .then_ignore(end())
        .map(move |items| File {
            name: file_name.clone(),
            items,
        })
        .labelled("file")
}

pub fn parse<'src, I>(lexer: I, len: usize, file_name: PathBuf) -> (Option<File>, Vec<Error<'src>>)
where
    I: 'src,
    I: Iterator<Item = (Token<'src>, Span)>,
{
    file_parser(file_name).parse_recovery_verbose(Stream::from_iter(len..len + 1, lexer))
}

#[cfg(test)]
mod tests {
    use std::{fmt::Debug, path::PathBuf};

    use logos::Logos;

    use crate::lexer::Token;

    fn parse(src: &str) -> impl Debug + '_ {
        let lexer = Token::lexer(src);
        let len = lexer.source().len();

        super::parse(
            lexer.spanned(),
            len,
            PathBuf::from(module_path!().replace("::", "__")),
        )
    }

    #[test]
    fn addition() {
        let r = parse("fn main() { 1 + 4; }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn expression() {
        let r = parse("fn main() { (4 / hallo()) + 5; }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn unary() {
        let r = parse(
            "fn main() {
    -(*5);
    &5;
    2 + &8;
    *6 * *8; // :)
}",
        );
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn function() {
        let r = parse("fn foo() -> u64 { 1 + 5; }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn if_no_else() {
        let r = parse("fn foo() -> u64 { if false {} }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn if_else() {
        let r = parse("fn foo() -> u64 { if false {} else {} }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn while_loop() {
        let r = parse("fn foo() -> u64 { while false {} }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn var_decl() {
        let r = parse("fn foo() -> u64 { u64 hello = 5; }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn struct_() {
        let r = parse("struct X { y: u64, x: u64 }");
        insta::assert_debug_snapshot!(r);
    }

    #[test]
    fn types() {
        let r = parse("fn types() -> ptr u64 { Test test = 2; ptr u64 int = 25; }");
        insta::assert_debug_snapshot!(r);
    }
}
