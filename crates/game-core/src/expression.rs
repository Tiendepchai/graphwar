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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryFunction {
    Sqrt,
    Log10,
    Ln,
    Abs,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Exp,
    Floor,
    Ceil,
    Sign,
    Sec,
    Csc,
    Cot,
    Asec,
    Acsc,
    Acot,
}

impl UnaryFunction {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sqrt => "sqrt",
            Self::Log10 => "log",
            Self::Ln => "ln",
            Self::Abs => "abs",
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Tan => "tan",
            Self::Asin => "asin",
            Self::Acos => "acos",
            Self::Atan => "atan",
            Self::Sinh => "sinh",
            Self::Cosh => "cosh",
            Self::Tanh => "tanh",
            Self::Exp => "exp",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Sign => "sign",
            Self::Sec => "sec",
            Self::Csc => "csc",
            Self::Cot => "cot",
            Self::Asec => "asec",
            Self::Acsc => "acsc",
            Self::Acot => "acot",
        }
    }

    fn evaluate(self, value: f64) -> f64 {
        match self {
            Self::Sqrt => value.sqrt(),
            Self::Log10 => value.log10(),
            Self::Ln => value.ln(),
            Self::Abs => value.abs(),
            Self::Sin => value.sin(),
            Self::Cos => value.cos(),
            Self::Tan => value.tan(),
            Self::Asin => value.asin(),
            Self::Acos => value.acos(),
            Self::Atan => value.atan(),
            Self::Sinh => value.sinh(),
            Self::Cosh => value.cosh(),
            Self::Tanh => value.tanh(),
            Self::Exp => value.exp(),
            Self::Floor => value.floor(),
            Self::Ceil => value.ceil(),
            Self::Sign => value.signum(),
            Self::Sec => 1.0 / value.cos(),
            Self::Csc => 1.0 / value.sin(),
            Self::Cot => 1.0 / value.tan(),
            Self::Asec => (1.0 / value).acos(),
            Self::Acsc => (1.0 / value).asin(),
            Self::Acot => 1.0_f64.atan2(value),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryFunction {
    Min,
    Max,
    Atan2,
    Log,
}

impl BinaryFunction {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
            Self::Atan2 => "atan2",
            Self::Log => "log",
        }
    }

    fn evaluate(self, left: f64, right: f64) -> f64 {
        match self {
            Self::Min if left.is_nan() || right.is_nan() => f64::NAN,
            Self::Min => left.min(right),
            Self::Max if left.is_nan() || right.is_nan() => f64::NAN,
            Self::Max => left.max(right),
            Self::Atan2 => left.atan2(right),
            Self::Log => left.ln() / right.ln(),
        }
    }
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
    Unary(UnaryFunction, Box<Self>),
    Binary(BinaryFunction, Box<Self>, Box<Self>),
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
            Self::Unary(function, value) => function.evaluate(value.evaluate(vars)),
            Self::Binary(function, left, right) => {
                function.evaluate(left.evaluate(vars), right.evaluate(vars))
            }
        }
    }

    pub fn evaluate_finite(&self, vars: EvalVars) -> Option<f64> {
        let result = self.evaluate(vars);
        result.is_finite().then_some(result)
    }

    pub fn uses_y(&self) -> bool {
        match self {
            Self::Y => true,
            Self::Neg(value) | Self::Unary(_, value) => value.uses_y(),
            Self::Add(left, right)
            | Self::Mul(left, right)
            | Self::Div(left, right)
            | Self::Pow(left, right)
            | Self::Binary(_, left, right) => left.uses_y() || right.uses_y(),
            Self::Number(_) | Self::X | Self::Dy => false,
        }
    }

    pub fn uses_dy(&self) -> bool {
        match self {
            Self::Dy => true,
            Self::Neg(value) | Self::Unary(_, value) => value.uses_dy(),
            Self::Add(left, right)
            | Self::Mul(left, right)
            | Self::Div(left, right)
            | Self::Pow(left, right)
            | Self::Binary(_, left, right) => left.uses_dy() || right.uses_dy(),
            Self::Number(_) | Self::X | Self::Y => false,
        }
    }

    pub fn variables_allowed(&self, allows_y: bool, allows_dy: bool) -> bool {
        (!self.uses_y() || allows_y) && (!self.uses_dy() || allows_dy)
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct NumberToken {
    value: f64,
    digits: usize,
    has_decimal: bool,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(NumberToken),
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
    Comma,
    Semicolon,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Function {
    Unary(UnaryFunction),
    Binary(BinaryFunction),
    Log,
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

const MAX_INPUT_BYTES: usize = 256;
const MAX_NORMALIZED_BYTES: usize = 1024;
const MAX_TOKENS: usize = 128;
const MAX_AST_DEPTH: usize = 64;

struct Normalized {
    text: String,
    offsets: Vec<usize>,
    end_offset: usize,
}

struct Normalizer<'a> {
    input: &'a str,
    position: usize,
    end: usize,
    text: String,
    offsets: Vec<usize>,
    open_bars: usize,
}

impl<'a> Normalizer<'a> {
    fn new(input: &'a str, position: usize, end: usize) -> Self {
        Self {
            input,
            position,
            end,
            text: String::new(),
            offsets: Vec::new(),
            open_bars: 0,
        }
    }

    fn finish(mut self) -> Result<Normalized, ParseError> {
        self.normalize_until(None)?;
        if self.open_bars != 0 {
            return Err(self.error(self.end, "unclosed absolute value"));
        }
        Ok(Normalized {
            text: self.text,
            offsets: self.offsets,
            end_offset: self.end,
        })
    }

    fn error(&self, offset: usize, message: &'static str) -> ParseError {
        ParseError { offset, message }
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.position..self.end)?.chars().next()
    }

    fn bump(&mut self) -> Option<(usize, char)> {
        let offset = self.position;
        let character = self.peek()?;
        self.position += character.len_utf8();
        Some((offset, character))
    }

    fn push(&mut self, text: &str, offset: usize) -> Result<(), ParseError> {
        if self.text.len().saturating_add(text.len()) > MAX_NORMALIZED_BYTES {
            return Err(self.error(offset, "normalized expression is too long"));
        }
        self.text.push_str(text);
        self.offsets.extend(std::iter::repeat_n(offset, text.len()));
        Ok(())
    }

    fn push_piece(
        &mut self,
        text: &str,
        offsets: &[usize],
        fallback_offset: usize,
    ) -> Result<(), ParseError> {
        if self.text.len().saturating_add(text.len()) > MAX_NORMALIZED_BYTES {
            return Err(self.error(fallback_offset, "normalized expression is too long"));
        }
        self.text.push_str(text);
        self.offsets.extend_from_slice(offsets);
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn normalize_until(&mut self, terminator: Option<char>) -> Result<usize, ParseError> {
        while let Some(character) = self.peek() {
            if Some(character) == terminator {
                return Ok(self.bump().expect("peeked character exists").0);
            }
            let (offset, character) = self.bump().expect("peeked character exists");
            match character {
                '(' => self.normalize_group(offset, ')')?,
                '{' => self.normalize_group(offset, '}')?,
                '[' => self.normalize_group(offset, ']')?,
                ')' | '}' | ']' => {
                    return Err(self.error(offset, "unexpected closing delimiter"));
                }
                '\\' => self.normalize_command(offset)?,
                '|' => self.normalize_bar(offset, None)?,
                'π' => self.push("pi", offset)?,
                '−' => self.push("-", offset)?,
                '×' | '·' => self.push("*", offset)?,
                '÷' => self.push("/", offset)?,
                '′' => self.push("'", offset)?,
                '=' => return Err(self.error(offset, "unexpected equals sign")),
                value if value.is_whitespace() => self.push(" ", offset)?,
                value if value.is_ascii() => self.push(&value.to_string(), offset)?,
                _ => return Err(self.error(offset, "invalid character")),
            }
        }
        terminator.map_or(Ok(self.end), |_| {
            Err(self.error(self.end, "expected closing delimiter"))
        })
    }

    fn normalize_group(&mut self, opening_offset: usize, closing: char) -> Result<(), ParseError> {
        self.push("(", opening_offset)?;
        let closing_offset = self.normalize_until(Some(closing))?;
        self.push(")", closing_offset)
    }

    fn capture_group(&mut self, opening: char, closing: char) -> Result<Normalized, ParseError> {
        self.skip_whitespace();
        if self.peek() != Some(opening) {
            return Err(self.error(self.position, "expected grouped argument"));
        }
        self.bump();
        let previous_text = std::mem::take(&mut self.text);
        let previous_offsets = std::mem::take(&mut self.offsets);
        let result = self.normalize_until(Some(closing));
        let text = std::mem::take(&mut self.text);
        let offsets = std::mem::take(&mut self.offsets);
        self.text = previous_text;
        self.offsets = previous_offsets;
        let closing_offset = result?;
        Ok(Normalized {
            text,
            offsets,
            end_offset: closing_offset,
        })
    }

    fn capture_any_group(&mut self) -> Result<Normalized, ParseError> {
        self.skip_whitespace();
        match self.peek() {
            Some('{') => self.capture_group('{', '}'),
            Some('(') => self.capture_group('(', ')'),
            Some('[') => self.capture_group('[', ']'),
            _ => Err(self.error(self.position, "expected grouped argument")),
        }
    }

    fn normalize_command(&mut self, offset: usize) -> Result<(), ParseError> {
        let Some(character) = self.peek() else {
            return Err(self.error(offset, "incomplete LaTeX command"));
        };
        if !character.is_ascii_alphabetic() {
            self.bump();
            return match character {
                ',' | ';' | ':' | '!' | ' ' => Ok(()),
                '(' | '[' => self.push("(", offset),
                ')' | ']' => self.push(")", offset),
                '{' => self.push("(", offset),
                '}' => self.push(")", offset),
                '|' => self.normalize_bar(offset, None),
                _ => Err(self.error(offset, "unsupported LaTeX command")),
            };
        }

        let start = self.position;
        while self.peek().is_some_and(|value| value.is_ascii_alphabetic()) {
            self.bump();
        }
        let command = self.input[start..self.position].to_ascii_lowercase();
        match command.as_str() {
            "left" | "right" | "quad" | "qquad" | "enspace" | "thinspace" => Ok(()),
            "cdot" | "times" => self.push("*", offset),
            "div" => self.push("/", offset),
            "pi" => self.push("pi", offset),
            "prime" => self.push("'", offset),
            "frac" | "dfrac" | "tfrac" => self.normalize_fraction(offset),
            "sqrt" => self.normalize_root(offset),
            "operatorname" | "mathrm" => self.normalize_identifier_group(offset),
            "log" => self.normalize_log(offset),
            "lfloor" => {
                self.push("floor(", offset)?;
                self.open_bars += 1;
                Ok(())
            }
            "rfloor" => self.close_named_delimiter(offset),
            "lceil" => {
                self.push("ceil(", offset)?;
                self.open_bars += 1;
                Ok(())
            }
            "rceil" => self.close_named_delimiter(offset),
            "lvert" | "vert" => self.normalize_bar(offset, Some(true)),
            "rvert" => self.normalize_bar(offset, Some(false)),
            _ => {
                let Some(name) = latex_function_name(&command) else {
                    return Err(self.error(offset, "unsupported LaTeX command"));
                };
                let inverse = self.consume_inverse_exponent()?;
                let name = if inverse {
                    inverse_function_name(name).ok_or_else(|| {
                        self.error(offset, "inverse exponent is unsupported for this function")
                    })?
                } else {
                    name
                };
                self.push(name, offset)
            }
        }
    }

    fn normalize_fraction(&mut self, offset: usize) -> Result<(), ParseError> {
        let numerator = self.capture_group('{', '}')?;
        let denominator = self.capture_group('{', '}')?;
        self.push("((", offset)?;
        self.push_piece(&numerator.text, &numerator.offsets, offset)?;
        self.push(")/(", offset)?;
        self.push_piece(&denominator.text, &denominator.offsets, offset)?;
        self.push("))", offset)
    }

    fn normalize_root(&mut self, offset: usize) -> Result<(), ParseError> {
        let saved = self.position;
        self.skip_whitespace();
        if self.peek() != Some('[') {
            self.position = saved;
            return self.push("sqrt", offset);
        }
        let index = self.capture_group('[', ']')?;
        let radicand = self.capture_group('{', '}')?;
        self.push("((", offset)?;
        self.push_piece(&radicand.text, &radicand.offsets, offset)?;
        self.push(")^(1/(", offset)?;
        self.push_piece(&index.text, &index.offsets, offset)?;
        self.push(")))", offset)
    }

    fn normalize_identifier_group(&mut self, offset: usize) -> Result<(), ParseError> {
        let identifier = self.capture_group('{', '}')?;
        let compact = identifier
            .text
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        if compact.is_empty()
            || !compact
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '\'')
        {
            return Err(self.error(offset, "invalid LaTeX identifier"));
        }
        self.push(&compact, offset)
    }

    fn normalize_log(&mut self, offset: usize) -> Result<(), ParseError> {
        let saved = self.position;
        self.skip_whitespace();
        if self.peek() != Some('_') {
            self.position = saved;
            return self.push("log", offset);
        }
        self.bump();
        let base = self.capture_any_group()?;
        self.skip_whitespace();
        if self.input[self.position..self.end].starts_with("\\left") {
            self.position += "\\left".len();
            self.skip_whitespace();
        }
        let value = self.capture_any_group()?;
        self.push("log(", offset)?;
        self.push_piece(&value.text, &value.offsets, offset)?;
        self.push(",", offset)?;
        self.push_piece(&base.text, &base.offsets, offset)?;
        self.push(")", offset)
    }

    fn consume_inverse_exponent(&mut self) -> Result<bool, ParseError> {
        let saved = self.position;
        self.skip_whitespace();
        if self.peek() != Some('^') {
            self.position = saved;
            return Ok(false);
        }
        let offset = self.bump().expect("caret exists").0;
        self.skip_whitespace();
        let exponent = if self.peek() == Some('{') {
            self.capture_group('{', '}')?.text
        } else {
            let start = self.position;
            if self.peek() == Some('-') {
                self.bump();
            }
            if self.peek() == Some('1') {
                self.bump();
            }
            self.input[start..self.position].to_owned()
        };
        let exponent = exponent.replace(' ', "");
        if exponent != "-1" {
            return Err(self.error(offset, "only inverse function exponent -1 is supported"));
        }
        Ok(true)
    }

    fn normalize_bar(&mut self, offset: usize, opening: Option<bool>) -> Result<(), ParseError> {
        let opening = opening.unwrap_or(self.open_bars == 0);
        if opening {
            self.open_bars += 1;
            self.push("abs(", offset)
        } else {
            self.close_named_delimiter(offset)
        }
    }

    fn close_named_delimiter(&mut self, offset: usize) -> Result<(), ParseError> {
        if self.open_bars == 0 {
            return Err(self.error(offset, "unexpected closing delimiter"));
        }
        self.open_bars -= 1;
        self.push(")", offset)
    }
}

fn latex_function_name(command: &str) -> Option<&'static str> {
    Some(match command {
        "sin" | "sen" => "sin",
        "cos" => "cos",
        "tan" | "tg" => "tan",
        "arcsin" | "asin" => "asin",
        "arccos" | "acos" => "acos",
        "arctan" | "atan" => "atan",
        "sinh" => "sinh",
        "cosh" => "cosh",
        "tanh" => "tanh",
        "exp" => "exp",
        "abs" => "abs",
        "ln" => "ln",
        "floor" => "floor",
        "ceil" => "ceil",
        "sign" | "sgn" => "sign",
        "sec" => "sec",
        "csc" => "csc",
        "cot" => "cot",
        "arcsec" | "asec" => "asec",
        "arccsc" | "acsc" => "acsc",
        "arccot" | "acot" => "acot",
        "min" => "min",
        "max" => "max",
        "atan2" => "atan2",
        _ => return None,
    })
}

fn inverse_function_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "sin" => "asin",
        "cos" => "acos",
        "tan" => "atan",
        "sec" => "asec",
        "csc" => "acsc",
        "cot" => "acot",
        _ => return None,
    })
}

fn trim_bounds(input: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    let leading = input[start..end].trim_start();
    start += input[start..end].len() - leading.len();
    let trailing = input[start..end].trim_end();
    end = start + trailing.len();
    (start, end)
}

fn expression_bounds(input: &str) -> Result<(usize, usize), ParseError> {
    let (mut start, mut end) = trim_bounds(input, 0, input.len());
    let slice = &input[start..end];
    let delimiters = if slice.starts_with("$$") {
        Some((2, "$$"))
    } else if slice.starts_with('$') {
        Some((1, "$"))
    } else if slice.starts_with("\\(") {
        Some((2, "\\)"))
    } else if slice.starts_with("\\[") {
        Some((2, "\\]"))
    } else {
        None
    };
    if let Some((opening_size, closing)) = delimiters {
        if !slice.ends_with(closing) || slice.len() < opening_size + closing.len() {
            return Err(ParseError {
                offset: start,
                message: "unclosed math delimiter",
            });
        }
        start += opening_size;
        end -= closing.len();
        (start, end) = trim_bounds(input, start, end);
    }

    let mut depth = 0_i32;
    let mut equals = None;
    for (relative, character) in input[start..end].char_indices() {
        match character {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' => depth -= 1,
            '=' if depth == 0 && equals.replace(start + relative).is_some() => {
                return Err(ParseError {
                    offset: start + relative,
                    message: "too many equals signs",
                });
            }
            _ => {}
        }
    }
    if let Some(equals) = equals {
        let lhs = input[start..equals]
            .chars()
            .filter(|character| !character.is_whitespace())
            .map(|character| if character == '′' { '\'' } else { character })
            .collect::<String>();
        if !matches!(lhs.as_str(), "y" | "y'" | "y''" | "f(x)") {
            return Err(ParseError {
                offset: start,
                message: "unsupported equation left-hand side",
            });
        }
        (start, end) = trim_bounds(input, equals + 1, end);
    }
    Ok((start, end))
}

fn normalize(input: &str) -> Result<Normalized, ParseError> {
    if input.len() > MAX_INPUT_BYTES {
        return Err(ParseError {
            offset: MAX_INPUT_BYTES,
            message: "expression is too long",
        });
    }
    let (start, end) = expression_bounds(input)?;
    Normalizer::new(input, start, end).finish()
}

struct Parser {
    tokens: Vec<(Token, usize)>,
    index: usize,
    depth: usize,
    decimal_comma: bool,
}

pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let normalized = normalize(input)?;
    let mut parser = Parser {
        tokens: lex_tokens(&normalized)?,
        index: 0,
        depth: 0,
        decimal_comma: true,
    };
    let expression = parser.sum()?;
    if !matches!(parser.current().0, Token::End) {
        return Err(parser.error("unexpected token"));
    }
    Ok(expression)
}

pub fn lex(input: &str) -> Result<Vec<(String, usize)>, ParseError> {
    let normalized = normalize(input)?;
    lex_tokens(&normalized).map(|tokens| {
        tokens
            .into_iter()
            .filter_map(|(token, offset)| match token {
                Token::End => None,
                other => Some((format!("{other:?}"), offset)),
            })
            .collect()
    })
}

fn lex_tokens(input: &Normalized) -> Result<Vec<(Token, usize)>, ParseError> {
    let mut tokens = Vec::new();
    let bytes = input.text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        let offset = input.offsets[index];
        let single = match bytes[index] {
            b'+' => Some(Token::Add),
            b'-' => Some(Token::Minus),
            b'*' => Some(Token::Mul),
            b'/' => Some(Token::Div),
            b'^' => Some(Token::Pow),
            b'(' => Some(Token::Left),
            b')' => Some(Token::Right),
            b',' => Some(Token::Comma),
            b';' => Some(Token::Semicolon),
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
            while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
                if bytes[index] == b'.' {
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
            let raw = &input.text[start..index];
            let value = raw.parse::<f64>().map_err(|_| ParseError {
                offset,
                message: "invalid number",
            })?;
            if !value.is_finite() {
                return Err(ParseError {
                    offset,
                    message: "number is not finite",
                });
            }
            tokens.push((
                Token::Number(NumberToken {
                    value,
                    digits: raw.bytes().filter(u8::is_ascii_digit).count(),
                    has_decimal: dots != 0,
                }),
                offset,
            ));
            continue;
        }
        if bytes[index].is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
                index += 1;
            }
            if input.text[start..].starts_with("y'") {
                index = start + 2;
            }
            let word = &input.text[start..index];
            let token = identifier_token(word).ok_or(ParseError {
                offset,
                message: "unknown identifier",
            })?;
            tokens.push((token, offset));
            continue;
        }
        return Err(ParseError {
            offset,
            message: "invalid character",
        });
    }
    tokens.push((Token::End, input.end_offset));
    if tokens.len() > MAX_TOKENS {
        return Err(ParseError {
            offset: input.end_offset,
            message: "expression has too many tokens",
        });
    }
    Ok(tokens)
}

fn identifier_token(word: &str) -> Option<Token> {
    Some(match word.to_ascii_lowercase().as_str() {
        "x" => Token::X,
        "y" => Token::Y,
        "y'" => Token::Dy,
        "e" => Token::Constant(std::f64::consts::E),
        "pi" => Token::Constant(std::f64::consts::PI),
        "sqrt" => Token::Function(Function::Unary(UnaryFunction::Sqrt)),
        "ln" => Token::Function(Function::Unary(UnaryFunction::Ln)),
        "abs" => Token::Function(Function::Unary(UnaryFunction::Abs)),
        "sin" | "sen" => Token::Function(Function::Unary(UnaryFunction::Sin)),
        "cos" => Token::Function(Function::Unary(UnaryFunction::Cos)),
        "tan" | "tg" => Token::Function(Function::Unary(UnaryFunction::Tan)),
        "asin" | "arcsin" => Token::Function(Function::Unary(UnaryFunction::Asin)),
        "acos" | "arccos" => Token::Function(Function::Unary(UnaryFunction::Acos)),
        "atan" | "arctan" => Token::Function(Function::Unary(UnaryFunction::Atan)),
        "sinh" => Token::Function(Function::Unary(UnaryFunction::Sinh)),
        "cosh" => Token::Function(Function::Unary(UnaryFunction::Cosh)),
        "tanh" => Token::Function(Function::Unary(UnaryFunction::Tanh)),
        "exp" => Token::Function(Function::Unary(UnaryFunction::Exp)),
        "floor" => Token::Function(Function::Unary(UnaryFunction::Floor)),
        "ceil" => Token::Function(Function::Unary(UnaryFunction::Ceil)),
        "sign" | "sgn" => Token::Function(Function::Unary(UnaryFunction::Sign)),
        "sec" => Token::Function(Function::Unary(UnaryFunction::Sec)),
        "csc" => Token::Function(Function::Unary(UnaryFunction::Csc)),
        "cot" => Token::Function(Function::Unary(UnaryFunction::Cot)),
        "asec" | "arcsec" => Token::Function(Function::Unary(UnaryFunction::Asec)),
        "acsc" | "arccsc" => Token::Function(Function::Unary(UnaryFunction::Acsc)),
        "acot" | "arccot" => Token::Function(Function::Unary(UnaryFunction::Acot)),
        "min" => Token::Function(Function::Binary(BinaryFunction::Min)),
        "max" => Token::Function(Function::Binary(BinaryFunction::Max)),
        "atan2" => Token::Function(Function::Binary(BinaryFunction::Atan2)),
        "log" => Token::Function(Function::Log),
        _ => return None,
    })
}

impl Parser {
    fn current(&self) -> &(Token, usize) {
        &self.tokens[self.index]
    }

    fn peek_token(&self, distance: usize) -> Option<&Token> {
        self.tokens.get(self.index + distance).map(|value| &value.0)
    }

    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.current().1,
            message,
        }
    }

    fn eat(&mut self) -> Token {
        let value = self.current().0.clone();
        if !matches!(value, Token::End) {
            self.index += 1;
        }
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
                    expr = Expr::Mul(Box::new(expr), Box::new(self.power()?));
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
        self.depth += 1;
        if self.depth > MAX_AST_DEPTH {
            self.depth -= 1;
            return Err(self.error("expression is too deeply nested"));
        }
        let result = self.primary_inner();
        self.depth -= 1;
        result
    }

    fn primary_inner(&mut self) -> Result<Expr, ParseError> {
        match self.eat() {
            Token::Number(value) => self.number(value),
            Token::Constant(value) => Ok(Expr::Number(value)),
            Token::X => Ok(Expr::X),
            Token::Y => Ok(Expr::Y),
            Token::Dy => Ok(Expr::Dy),
            Token::Function(function) => self.function(function),
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

    fn number(&mut self, integer: NumberToken) -> Result<Expr, ParseError> {
        if self.decimal_comma
            && !integer.has_decimal
            && matches!(self.current().0, Token::Comma)
            && let Some(Token::Number(fraction)) = self.peek_token(1)
            && !fraction.has_decimal
        {
            let fraction = *fraction;
            self.eat();
            self.eat();
            let scale = 10.0_f64.powi(fraction.digits.try_into().unwrap_or(i32::MAX));
            return Ok(Expr::Number(integer.value + fraction.value / scale));
        }
        Ok(Expr::Number(integer.value))
    }

    fn function(&mut self, function: Function) -> Result<Expr, ParseError> {
        match function {
            Function::Unary(function) => {
                let value = self.unary_argument()?;
                Ok(Expr::Unary(function, Box::new(value)))
            }
            Function::Binary(function) => {
                let arguments = self.arguments()?;
                if arguments.len() != 2 {
                    return Err(self.error("function expects two arguments"));
                }
                let mut arguments = arguments.into_iter();
                Ok(Expr::Binary(
                    function,
                    Box::new(arguments.next().expect("two arguments")),
                    Box::new(arguments.next().expect("two arguments")),
                ))
            }
            Function::Log if matches!(self.current().0, Token::Left) => {
                let arguments = self.arguments()?;
                match arguments.as_slice() {
                    [value] => Ok(Expr::Unary(UnaryFunction::Log10, Box::new(value.clone()))),
                    [value, base] => Ok(Expr::Binary(
                        BinaryFunction::Log,
                        Box::new(value.clone()),
                        Box::new(base.clone()),
                    )),
                    _ => Err(self.error("log expects one or two arguments")),
                }
            }
            Function::Log => Ok(Expr::Unary(
                UnaryFunction::Log10,
                Box::new(self.unary_argument()?),
            )),
        }
    }

    fn unary_argument(&mut self) -> Result<Expr, ParseError> {
        if !matches!(self.current().0, Token::Left) {
            return self.unary();
        }
        self.eat();
        let previous = self.decimal_comma;
        self.decimal_comma = true;
        let value = self.sum();
        self.decimal_comma = previous;
        let value = value?;
        if !matches!(self.current().0, Token::Right) {
            return Err(self.error("function expects one argument"));
        }
        self.eat();
        Ok(value)
    }

    fn arguments(&mut self) -> Result<Vec<Expr>, ParseError> {
        if !matches!(self.current().0, Token::Left) {
            return Err(self.error("function requires parentheses"));
        }
        let semicolon = self.group_has_top_level_semicolon();
        self.eat();
        let previous = self.decimal_comma;
        self.decimal_comma = semicolon;
        let separator = if semicolon {
            Token::Semicolon
        } else {
            Token::Comma
        };
        let mut arguments = Vec::new();
        loop {
            if matches!(self.current().0, Token::Right) {
                break;
            }
            arguments.push(self.sum()?);
            if std::mem::discriminant(&self.current().0) == std::mem::discriminant(&separator) {
                self.eat();
                if matches!(self.current().0, Token::Right) {
                    self.decimal_comma = previous;
                    return Err(self.error("trailing argument separator"));
                }
                continue;
            }
            break;
        }
        self.decimal_comma = previous;
        if !matches!(self.current().0, Token::Right) {
            return Err(self.error("expected closing parenthesis"));
        }
        self.eat();
        Ok(arguments)
    }

    fn group_has_top_level_semicolon(&self) -> bool {
        let mut depth = 0_usize;
        for (token, _) in self.tokens.iter().skip(self.index + 1) {
            match token {
                Token::Left => depth += 1,
                Token::Right if depth == 0 => return false,
                Token::Right => depth -= 1,
                Token::Semicolon if depth == 0 => return true,
                _ => {}
            }
        }
        false
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

    fn assert_near(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-10, "{actual} != {expected}");
    }

    #[test]
    fn aliases_implicit_multiplication_and_variables() {
        assert_near(value("2x + sen(pi/2)y + tg(0) + y'"), 15.0);
        assert_near(value("2(3+x) + exp(1)"), 12.0 + std::f64::consts::E);
    }

    #[test]
    fn evaluates_all_unary_functions_and_aliases() {
        let cases = [
            ("asin(0.5)", 0.5_f64.asin()),
            ("arccos(0.5)", 0.5_f64.acos()),
            ("arctan(0.5)", 0.5_f64.atan()),
            ("sinh(0.5)", 0.5_f64.sinh()),
            ("cosh(0.5)", 0.5_f64.cosh()),
            ("tanh(0.5)", 0.5_f64.tanh()),
            ("floor(1.9)", 1.0),
            ("ceil(1.1)", 2.0),
            ("sign(-2)", -1.0),
            ("sec(0)", 1.0),
            ("csc(pi/2)", 1.0),
            ("cot(pi/4)", 1.0),
            ("arcsec(2)", 0.5_f64.acos()),
            ("arccsc(2)", 0.5_f64.asin()),
            ("arccot(1)", std::f64::consts::FRAC_PI_4),
        ];
        for (input, expected) in cases {
            assert_near(value(input), expected);
        }
    }

    #[test]
    fn binary_functions_and_decimal_separators_are_unambiguous() {
        assert_near(value("min(1,5)"), 1.0);
        assert_near(value("max(1.5,2.5)"), 2.5);
        assert_near(value("atan2(1,1)"), std::f64::consts::FRAC_PI_4);
        assert_near(value("log(8,2)"), 3.0);
        assert_near(value("log(100)"), 2.0);
        assert_near(value("1,5 + sin(0,5)"), 1.5 + 0.5_f64.sin());
        assert_near(value("max(1,5;2,5)"), 2.5);
    }

    #[test]
    fn latex_and_plain_text_have_the_same_value() {
        assert_near(value(r"$\frac{\sin(\pi/2)+\sqrt[3]{8}}{\log_{2}(8)}$"), 1.0);
        assert_near(
            value(r"\left|\cos^{-1}(0)\right|"),
            std::f64::consts::FRAC_PI_2,
        );
        assert_near(value(r"\operatorname{max}\left(2,3\right)"), 3.0);
        assert_near(value("y = π×x − 1"), 3.0 * std::f64::consts::PI - 1.0);
        assert_near(value(r"f(x)=\mathrm{e}^{0}+x"), 4.0);
    }

    #[test]
    fn strict_parser_rejects_malformed_or_unsupported_input() {
        for input in [
            "x@2",
            "sinx",
            "y''",
            "(x",
            "min(1)",
            "atan2(1,2,3)",
            "log(1,)",
            r"\unknown{x}",
            r"\frac{x}{",
            r"\sin^{-2}(x)",
            "z=x",
        ] {
            assert!(parse(input).is_err(), "input should be rejected: {input:?}");
        }
    }

    #[test]
    fn incomplete_expressions_return_errors_without_panicking() {
        for input in [
            "", "+", "-", "x+", "x*", "x/", "x^", "sin", "sin(", "sqrt(", "(",
        ] {
            assert!(parse(input).is_err(), "input should be rejected: {input:?}");
        }
    }

    #[test]
    fn parser_budgets_reject_adversarial_input() {
        assert!(parse(&"x+".repeat(MAX_TOKENS)).is_err());
        assert!(parse(&format!("{}x{}", "(".repeat(65), ")".repeat(65))).is_err());
        assert!(parse(&"x".repeat(MAX_INPUT_BYTES + 1)).is_err());
    }

    #[test]
    fn normalization_errors_keep_original_offsets() {
        let error = parse(r"\frac{1}{2}+\wat{x}").unwrap_err();
        assert_eq!(error.offset, 12);
    }

    #[test]
    fn variable_usage_is_reported_recursively() {
        let expression = parse("max(sin(y), y')").unwrap();
        assert!(expression.uses_y());
        assert!(expression.uses_dy());
        assert!(!expression.variables_allowed(false, false));
        assert!(!expression.variables_allowed(true, false));
        assert!(expression.variables_allowed(true, true));
    }

    #[test]
    fn precedence_and_right_associative_power() {
        assert_eq!(value("-2^2"), 4.0);
        assert_eq!(value("2^3^2"), 512.0);
    }
}
