/*
 * Copyright 2022, The Cozo Project Authors. Licensed under MPL-2.0.
 */

use std::collections::BTreeMap;

use itertools::Itertools;
use lazy_static::lazy_static;
use miette::{bail, ensure, Diagnostic, Result};
use pest::pratt_parser::{Op, PrattParser};
use smartstring::{LazyCompact, SmartString};
use thiserror::Error;

use crate::data::expr::{get_op, Expr};
use crate::data::functions::{
    OP_ADD, OP_AND, OP_CONCAT, OP_DIV, OP_EQ, OP_GE, OP_GT, OP_LE, OP_LIST, OP_LT, OP_MINUS,
    OP_MOD, OP_MUL, OP_NEGATE, OP_NEQ, OP_OR, OP_POW, OP_SUB,
};
use crate::data::symb::Symbol;
use crate::data::value::DataValue;
use crate::parse::{ExtractSpan, Pair, Rule, SourceSpan};

lazy_static! {
    static ref PRATT_PARSER: PrattParser<Rule> = {
        use pest::pratt_parser::Assoc::*;

        PrattParser::new()
            .op(Op::infix(Rule::op_or, Left))
            .op(Op::infix(Rule::op_and, Left))
            .op(Op::infix(Rule::op_gt, Left)
                | Op::infix(Rule::op_lt, Left)
                | Op::infix(Rule::op_ge, Left)
                | Op::infix(Rule::op_le, Left))
            .op(Op::infix(Rule::op_mod, Left))
            .op(Op::infix(Rule::op_eq, Left) | Op::infix(Rule::op_ne, Left))
            .op(Op::infix(Rule::op_add, Left)
                | Op::infix(Rule::op_sub, Left)
                | Op::infix(Rule::op_concat, Left))
            .op(Op::infix(Rule::op_mul, Left) | Op::infix(Rule::op_div, Left))
            .op(Op::infix(Rule::op_pow, Right))
            .op(Op::prefix(Rule::minus))
            .op(Op::prefix(Rule::negate))
    };
}

#[derive(Debug, Error, Diagnostic)]
#[error("Invalid expression encountered")]
#[diagnostic(code(parser::invalid_expression))]
pub(crate) struct InvalidExpression(#[label] pub(crate) SourceSpan);

pub(crate) fn build_expr(pair: Pair<'_>, param_pool: &BTreeMap<String, DataValue>) -> Result<Expr> {
    ensure!(
        pair.as_rule() == Rule::expr,
        InvalidExpression(pair.extract_span())
    );

    PRATT_PARSER
        .map_primary(|v| build_term(v, param_pool))
        .map_infix(build_expr_infix)
        .map_prefix(|op, rhs| {
            let rhs = rhs?;
            let rhs_span = rhs.span();
            Ok(match op.as_rule() {
                Rule::minus => Expr::Apply {
                    op: &OP_MINUS,
                    args: [rhs].into(),
                    span: op.extract_span().merge(rhs_span),
                },
                Rule::negate => Expr::Apply {
                    op: &OP_NEGATE,
                    args: [rhs].into(),
                    span: op.extract_span().merge(rhs_span),
                },
                _ => unreachable!(),
            })
        })
        .parse(pair.into_inner())
}

fn build_expr_infix(lhs: Result<Expr>, op: Pair<'_>, rhs: Result<Expr>) -> Result<Expr> {
    let args = vec![lhs?, rhs?];
    let op = match op.as_rule() {
        Rule::op_add => &OP_ADD,
        Rule::op_sub => &OP_SUB,
        Rule::op_mul => &OP_MUL,
        Rule::op_div => &OP_DIV,
        Rule::op_mod => &OP_MOD,
        Rule::op_pow => &OP_POW,
        Rule::op_eq => &OP_EQ,
        Rule::op_ne => &OP_NEQ,
        Rule::op_gt => &OP_GT,
        Rule::op_ge => &OP_GE,
        Rule::op_lt => &OP_LT,
        Rule::op_le => &OP_LE,
        Rule::op_concat => &OP_CONCAT,
        Rule::op_or => &OP_OR,
        Rule::op_and => &OP_AND,
        _ => unreachable!(),
    };
    let start = args[0].span().0;
    let end = args[1].span().0 + args[1].span().1;
    let length = end - start;
    Ok(Expr::Apply {
        op,
        args: args.into(),
        span: SourceSpan(start, length),
    })
}

fn build_term(pair: Pair<'_>, param_pool: &BTreeMap<String, DataValue>) -> Result<Expr> {
    let span = pair.extract_span();
    let op = pair.as_rule();
    Ok(match op {
        Rule::var => Expr::Binding {
            var: Symbol::new(pair.as_str(), pair.extract_span()),
            tuple_pos: None,
        },
        Rule::param => {
            #[derive(Error, Diagnostic, Debug)]
            #[error("Required parameter {0} not found")]
            #[diagnostic(code(parser::param_not_found))]
            struct ParamNotFoundError(String, #[label] SourceSpan);

            let param_str = pair.as_str().strip_prefix('$').unwrap();
            Expr::Const {
                val: param_pool
                    .get(param_str)
                    .ok_or_else(|| ParamNotFoundError(param_str.to_string(), span))?
                    .clone(),
                span,
            }
        }
        Rule::pos_int => {
            #[derive(Error, Diagnostic, Debug)]
            #[error("Cannot parse integer")]
            #[diagnostic(code(parser::bad_pos_int))]
            struct BadIntError(#[label] SourceSpan);

            let i = pair
                .as_str()
                .replace('_', "")
                .parse::<i64>()
                .map_err(|_| BadIntError(span))?;
            Expr::Const {
                val: DataValue::from(i),
                span,
            }
        }
        Rule::hex_pos_int => {
            let i = parse_int(pair.as_str(), 16);
            Expr::Const {
                val: DataValue::from(i),
                span,
            }
        }
        Rule::octo_pos_int => {
            let i = parse_int(pair.as_str(), 8);
            Expr::Const {
                val: DataValue::from(i),
                span,
            }
        }
        Rule::bin_pos_int => {
            let i = parse_int(pair.as_str(), 2);
            Expr::Const {
                val: DataValue::from(i),
                span,
            }
        }
        Rule::dot_float | Rule::sci_float => {
            #[derive(Error, Diagnostic, Debug)]
            #[error("Cannot parse float")]
            #[diagnostic(code(parser::bad_float))]
            struct BadFloatError(#[label] SourceSpan);

            let f = pair
                .as_str()
                .replace('_', "")
                .parse::<f64>()
                .map_err(|_| BadFloatError(span))?;
            Expr::Const {
                val: DataValue::from(f),
                span,
            }
        }
        Rule::null => Expr::Const {
            val: DataValue::Null,
            span,
        },
        Rule::boolean => Expr::Const {
            val: DataValue::Bool(pair.as_str() == "true"),
            span,
        },
        Rule::quoted_string | Rule::s_quoted_string | Rule::raw_string => {
            let s = parse_string(pair)?;
            Expr::Const {
                val: DataValue::Str(s),
                span,
            }
        }
        Rule::list => {
            let mut collected = vec![];
            for p in pair.into_inner() {
                collected.push(build_expr(p, param_pool)?)
            }
            Expr::Apply {
                op: &OP_LIST,
                args: collected.into(),
                span,
            }
        }
        Rule::apply => {
            let mut p = pair.into_inner();
            let ident_p = p.next().unwrap();
            let ident = ident_p.as_str();
            let mut args: Vec<_> = p
                .next()
                .unwrap()
                .into_inner()
                .map(|v| build_expr(v, param_pool))
                .try_collect()?;
            #[derive(Error, Diagnostic, Debug)]
            #[error("Named function '{0}' not found")]
            #[diagnostic(code(parser::func_not_function))]
            struct FuncNotFoundError(String, #[label] SourceSpan);

            match ident {
                "try" => Expr::Try {
                    clauses: args,
                    span,
                },
                "cond" => {
                    if args.len() & 1 == 1 {
                        args.insert(
                            args.len() - 1,
                            Expr::Const {
                                val: DataValue::Bool(true),
                                span: args.last().unwrap().span(),
                            },
                        )
                    }
                    let clauses = args
                        .chunks(2)
                        .map(|pair| (pair[0].clone(), pair[1].clone()))
                        .collect_vec();
                    Expr::Cond { clauses, span }
                }
                "if" => {
                    #[derive(Debug, Error, Diagnostic)]
                    #[error("wrong number of arguments to if: 2 or 3 required")]
                    #[diagnostic(code(parser::bad_if))]
                    struct WrongArgsToIf(#[label] SourceSpan);

                    ensure!(args.len() == 2 || args.len() == 3, WrongArgsToIf(span));

                    let mut clauses = vec![];
                    let mut args = args.into_iter();
                    let cond = args.next().unwrap();
                    let then = args.next().unwrap();
                    clauses.push((cond, then));
                    if let Some(else_clause) = args.next() {
                        clauses.push((
                            Expr::Const {
                                val: DataValue::Bool(true),
                                span,
                            },
                            else_clause,
                        ))
                    }
                    Expr::Cond { clauses, span }
                }
                _ => {
                    let op = get_op(ident).ok_or_else(|| {
                        FuncNotFoundError(ident.to_string(), ident_p.extract_span())
                    })?;
                    op.post_process_args(&mut args);

                    #[derive(Error, Diagnostic, Debug)]
                    #[error("Wrong number of arguments for function '{0}'")]
                    #[diagnostic(code(parser::func_wrong_num_args))]
                    struct WrongNumArgsError(String, #[label] SourceSpan, #[help] String);

                    if op.vararg {
                        ensure!(
                            op.min_arity <= args.len(),
                            WrongNumArgsError(
                                ident.to_string(),
                                span,
                                format!("Need at least {} argument(s)", op.min_arity)
                            )
                        );
                    } else {
                        ensure!(
                            op.min_arity == args.len(),
                            WrongNumArgsError(
                                ident.to_string(),
                                span,
                                format!("Need exactly {} argument(s)", op.min_arity)
                            )
                        );
                    }
                    Expr::Apply {
                        op,
                        args: args.into(),
                        span,
                    }
                }
            }
        }
        Rule::grouping => build_expr(pair.into_inner().next().unwrap(), param_pool)?,
        r => unreachable!("Encountered unknown op {:?}", r),
    })
}

pub(crate) fn parse_int(s: &str, radix: u32) -> i64 {
    i64::from_str_radix(&s[2..].replace('_', ""), radix).unwrap()
}

pub(crate) fn parse_string(pair: Pair<'_>) -> Result<SmartString<LazyCompact>> {
    match pair.as_rule() {
        Rule::quoted_string => Ok(parse_quoted_string(pair)?),
        Rule::s_quoted_string => Ok(parse_s_quoted_string(pair)?),
        Rule::raw_string => Ok(parse_raw_string(pair)?),
        Rule::ident => Ok(SmartString::from(pair.as_str())),
        t => unreachable!("{:?}", t),
    }
}

#[derive(Error, Diagnostic, Debug)]
#[error("invalid UTF8 code {0}")]
#[diagnostic(code(parser::invalid_utf8_code))]
struct InvalidUtf8Error(u32, #[label] SourceSpan);

#[derive(Error, Diagnostic, Debug)]
#[error("invalid escape sequence {0}")]
#[diagnostic(code(parser::invalid_escape_seq))]
struct InvalidEscapeSeqError(String, #[label] SourceSpan);

fn parse_quoted_string(pair: Pair<'_>) -> Result<SmartString<LazyCompact>> {
    let pairs = pair.into_inner().next().unwrap().into_inner();
    let mut ret = SmartString::new();
    for pair in pairs {
        let s = pair.as_str();
        match s {
            r#"\""# => ret.push('"'),
            r"\\" => ret.push('\\'),
            r"\/" => ret.push('/'),
            r"\b" => ret.push('\x08'),
            r"\f" => ret.push('\x0c'),
            r"\n" => ret.push('\n'),
            r"\r" => ret.push('\r'),
            r"\t" => ret.push('\t'),
            s if s.starts_with(r"\u") => {
                let code = parse_int(s, 16) as u32;
                let ch = char::from_u32(code)
                    .ok_or_else(|| InvalidUtf8Error(code, pair.extract_span()))?;
                ret.push(ch);
            }
            s if s.starts_with('\\') => {
                bail!(InvalidEscapeSeqError(s.to_string(), pair.extract_span()))
            }
            s => ret.push_str(s),
        }
    }
    Ok(ret)
}

fn parse_s_quoted_string(pair: Pair<'_>) -> Result<SmartString<LazyCompact>> {
    let pairs = pair.into_inner().next().unwrap().into_inner();
    let mut ret = SmartString::new();
    for pair in pairs {
        let s = pair.as_str();
        match s {
            r#"\'"# => ret.push('\''),
            r"\\" => ret.push('\\'),
            r"\/" => ret.push('/'),
            r"\b" => ret.push('\x08'),
            r"\f" => ret.push('\x0c'),
            r"\n" => ret.push('\n'),
            r"\r" => ret.push('\r'),
            r"\t" => ret.push('\t'),
            s if s.starts_with(r"\u") => {
                let code = parse_int(s, 16) as u32;
                let ch = char::from_u32(code)
                    .ok_or_else(|| InvalidUtf8Error(code, pair.extract_span()))?;
                ret.push(ch);
            }
            s if s.starts_with('\\') => {
                bail!(InvalidEscapeSeqError(s.to_string(), pair.extract_span()))
            }
            s => ret.push_str(s),
        }
    }
    Ok(ret)
}

fn parse_raw_string(pair: Pair<'_>) -> Result<SmartString<LazyCompact>> {
    Ok(SmartString::from(
        pair.into_inner().into_iter().next().unwrap().as_str(),
    ))
}
