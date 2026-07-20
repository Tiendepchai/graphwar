// Copyright (C) 2026 Graphwar contributors
//
// This file is part of Graphwar. See COPYING for license terms.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EvalVars {
    pub x: f64,
    pub y: f64,
    pub dy: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Number(f64),
    X,
    Y,
    Dy,
    Neg(Box<Self>),
    Add(Box<Self>, Box<Self>),
    Mul(Box<Self>, Box<Self>),
    Div(Box<Self>, Box<Self>),
    Pow(Box<Self>, Box<Self>),
    Sqrt(Box<Self>),
    Log10(Box<Self>),
    Ln(Box<Self>),
    Abs(Box<Self>),
    Sin(Box<Self>),
    Cos(Box<Self>),
    Tan(Box<Self>),
}

pub type Ast = Expr;

impl Expr {
    pub fn evaluate(&self, vars: EvalVars) -> f64 {
        match self {
            Self::Number(n) => *n,
            Self::X => vars.x,
            Self::Y => vars.y,
            Self::Dy => vars.dy,
            Self::Neg(value) => -value.evaluate(vars),
            Self::Add(left, right) => left.evaluate(vars) + right.evaluate(vars),
            Self::Mul(left, right) => left.evaluate(vars) * right.evaluate(vars),
            Self::Div(left, right) => left.evaluate(vars) / right.evaluate(vars),
            Self::Pow(left, right) => left.evaluate(vars).powf(right.evaluate(vars)),
            Self::Sqrt(value) => value.evaluate(vars).sqrt(),
            Self::Log10(value) => value.evaluate(vars).log10(),
            Self::Ln(value) => value.evaluate(vars).ln(),
            Self::Abs(value) => value.evaluate(vars).abs(),
            Self::Sin(value) => value.evaluate(vars).sin(),
            Self::Cos(value) => value.evaluate(vars).cos(),
            Self::Tan(value) => value.evaluate(vars).tan(),
        }
    }

    pub fn evaluate_finite(&self, vars: EvalVars) -> Option<f64> {
        let result = self.evaluate(vars);
        result.is_finite().then_some(result)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.message, self.offset)
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    X,
    Y,
    Dy,
    Constant(f64),
    Function(Function),
    Add,
    Minus,
    Mul,
    Div,
    Pow,
    Left,
    Right,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Function {
    Sqrt,
    Log10,
    Ln,
    Abs,
    Sin,
    Cos,
    Tan,
}

impl Token {
    fn starts_factor(&self) -> bool {
        matches!(
            self,
            Self::Number(_)
                | Self::X
                | Self::Y
                | Self::Dy
                | Self::Constant(_)
                | Self::Function(_)
                | Self::Left
        )
    }
}

struct Parser {
    tokens: Vec<(Token, usize)>,
    index: usize,
}

pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let mut parser = Parser {
        tokens: lex_tokens(input)?,
        index: 0,
    };
    let expression = parser.sum()?;
    if !matches!(parser.current().0, Token::End) {
        return Err(parser.error("unexpected token"));
    }
    Ok(expression)
}

pub fn lex(input: &str) -> Result<Vec<(String, usize)>, ParseError> {
    lex_tokens(input).map(|tokens| {
        tokens
            .into_iter()
            .filter_map(|(token, offset)| match token {
                Token::End => None,
                other => Some((format!("{other:?}"), offset)),
            })
            .collect()
    })
}

fn lex_tokens(input: &str) -> Result<Vec<(Token, usize)>, ParseError> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let offset = index;
        let rest = &input[index..];
        let single = match bytes[index] {
            b'+' => Some(Token::Add),
            b'-' => Some(Token::Minus),
            b'*' => Some(Token::Mul),
            b'/' => Some(Token::Div),
            b'^' => Some(Token::Pow),
            b'(' => Some(Token::Left),
            b')' => Some(Token::Right),
            _ => None,
        };
        if let Some(token) = single {
            tokens.push((token, offset));
            index += 1;
            continue;
        }
        if bytes[index].is_ascii_digit() || bytes[index] == b'.' {
            let start = index;
            let mut dots = 0;
            while index < bytes.len()
                && (bytes[index].is_ascii_digit() || bytes[index] == b'.' || bytes[index] == b',')
            {
                if bytes[index] == b'.' || bytes[index] == b',' {
                    dots += 1;
                }
                index += 1;
            }
            if dots > 1 {
                return Err(ParseError {
                    offset,
                    message: "invalid number",
                });
            }
            let number = input[start..index]
                .replace(',', ".")
                .parse()
                .map_err(|_| ParseError {
                    offset,
                    message: "invalid number",
                })?;
            tokens.push((Token::Number(number), offset));
            continue;
        }
        let (word, size) = if rest.starts_with("y'") {
            ("y'", 2)
        } else {
            let size = rest
                .bytes()
                .take_while(|byte| byte.is_ascii_alphabetic())
                .count();
            if size == 0 {
                return Err(ParseError {
                    offset,
                    message: "invalid character",
                });
            }
            (&rest[..size], size)
        };
        index += size;
        let token = match word.to_ascii_lowercase().as_str() {
            "x" => Token::X,
            "y" => Token::Y,
            "y'" => Token::Dy,
            "e" => Token::Constant(std::f64::consts::E),
            "pi" => Token::Constant(std::f64::consts::PI),
            "sqrt" => Token::Function(Function::Sqrt),
            "log" => Token::Function(Function::Log10),
            "ln" => Token::Function(Function::Ln),
            "abs" => Token::Function(Function::Abs),
            "sin" | "sen" => Token::Function(Function::Sin),
            "cos" => Token::Function(Function::Cos),
            "tan" | "tg" => Token::Function(Function::Tan),
            "exp" => Token::Constant(std::f64::consts::E),
            _ => {
                return Err(ParseError {
                    offset,
                    message: "unknown identifier",
                })
            }
        };
        tokens.push((token, offset));
        if word.eq_ignore_ascii_case("exp") {
            tokens.push((Token::Pow, offset));
        }
    }
    tokens.push((Token::End, input.len()));
    Ok(tokens)
}

impl Parser {
    fn current(&self) -> &(Token, usize) {
        &self.tokens[self.index]
    }
    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.current().1,
            message,
        }
    }
    fn eat(&mut self) -> Token {
        let value = self.current().0.clone();
        self.index += 1;
        value
    }

    fn sum(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.product()?;
        loop {
            match self.current().0 {
                Token::Add => {
                    self.eat();
                    expr = Expr::Add(Box::new(expr), Box::new(self.product()?));
                }
                Token::Minus => {
                    self.eat();
                    expr = Expr::Add(
                        Box::new(expr),
                        Box::new(Expr::Neg(Box::new(self.product()?))),
                    );
                }
                _ => return Ok(expr),
            }
        }
    }

    fn product(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.power()?;
        loop {
            match self.current().0 {
                Token::Mul => {
                    self.eat();
                    expr = Expr::Mul(Box::new(expr), Box::new(self.power()?));
                }
                Token::Div => {
                    self.eat();
                    expr = Expr::Div(Box::new(expr), Box::new(self.power()?));
                }
                _ if self.current().0.starts_factor() => {
                    expr = Expr::Mul(Box::new(expr), Box::new(self.power()?))
                }
                _ => return Ok(expr),
            }
        }
    }

    fn power(&mut self) -> Result<Expr, ParseError> {
        let base = self.unary()?;
        if matches!(self.current().0, Token::Pow) {
            self.eat();
            Ok(Expr::Pow(Box::new(base), Box::new(self.power()?)))
        } else {
            Ok(base)
        }
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.current().0, Token::Minus) {
            self.eat();
            Ok(Expr::Neg(Box::new(self.unary()?)))
        } else {
            self.primary()
        }
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.eat() {
            Token::Number(value) | Token::Constant(value) => Ok(Expr::Number(value)),
            Token::X => Ok(Expr::X),
            Token::Y => Ok(Expr::Y),
            Token::Dy => Ok(Expr::Dy),
            Token::Function(function) => {
                let value = if matches!(self.current().0, Token::Left) {
                    self.eat();
                    let value = self.sum()?;
                    if !matches!(self.current().0, Token::Right) {
                        return Err(self.error("expected closing parenthesis"));
                    }
                    self.eat();
                    value
                } else {
                    self.unary()?
                };
                Ok(match function {
                    Function::Sqrt => Expr::Sqrt(Box::new(value)),
                    Function::Log10 => Expr::Log10(Box::new(value)),
                    Function::Ln => Expr::Ln(Box::new(value)),
                    Function::Abs => Expr::Abs(Box::new(value)),
                    Function::Sin => Expr::Sin(Box::new(value)),
                    Function::Cos => Expr::Cos(Box::new(value)),
                    Function::Tan => Expr::Tan(Box::new(value)),
                })
            }
            Token::Left => {
                let value = self.sum()?;
                if !matches!(self.current().0, Token::Right) {
                    return Err(self.error("expected closing parenthesis"));
                }
                self.eat();
                Ok(value)
            }
            _ => Err(self.error("expected expression")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(input: &str) -> f64 {
        parse(input).unwrap().evaluate(EvalVars {
            x: 3.0,
            y: 4.0,
            dy: 5.0,
        })
    }

    #[test]
    fn aliases_implicit_multiplication_and_variables() {
        assert!((value("2x + sen(pi/2)y + tg(0) + y'") - 15.0).abs() < 1e-12);
        assert!((value("2(3+x) + exp(1)") - (12.0 + std::f64::consts::E)).abs() < 1e-12);
    }

    #[test]
    fn strict_lexer_rejects_skipped_garbage() {
        assert!(parse("x@2").is_err());
        assert!(parse("sinx").is_err());
        assert!(parse("y''").is_err());
        assert!(parse("(x").is_err());
    }

    #[test]
    fn precedence_and_right_associative_power() {
        assert_eq!(value("-2^2"), 4.0);
        assert_eq!(value("2^3^2"), 512.0);
    }
}
