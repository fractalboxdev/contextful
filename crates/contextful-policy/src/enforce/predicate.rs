//! `authority.filter-rows`: the typed row-predicate grammar, parsed at manifest load and
//! compiled into SQL whose subject claims read from the session-scoped subject relation.

use contextful_core::enforce::EnforceError;
use contextful_core::store::relation::{ident, literal};

/// Source text of one row predicate, in bytes (`authority.filter-rows.predicate-size`).
pub const PREDICATE_SIZE: usize = 4 * 1024;

/// Literals in one membership list (`authority.filter-rows.membership-list`).
pub const MEMBERSHIP_LIST_ENTRIES: usize = 512;

/// The session-scoped relation subject claims reach a predicate through, one row whose
/// columns are the subject's members (`authority.filter-rows.subject-relation`).
pub const SUBJECT_RELATION: &str = "__contextful_subject";

/// The subject fields a predicate reads: the members of the subject tuple.
pub const SUBJECT_FIELDS: [&str; 5] = ["on_behalf_of", "agent", "host", "task", "zone"];

/// Scalar functions declared safe inside a predicate: pure, total over text, and
/// reaching no state outside their arguments.
pub const SAFE_FUNCTIONS: [&str; 5] = ["lower", "upper", "trim", "length", "coalesce"];

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Text(String),
    Number(String),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Column(String),
    Subject(String),
    Literal(Literal),
    Call(String, Vec<Operand>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A parsed predicate: the typed boolean subset of SQL (`authority.filter-rows.grammar`).
#[derive(Debug, Clone, PartialEq)]
pub enum Predicate {
    Or(Vec<Predicate>),
    And(Vec<Predicate>),
    Not(Box<Predicate>),
    Compare(Operand, Comparison, Operand),
    In { column: String, negated: bool, list: Vec<Literal> },
    Like { column: String, negated: bool, pattern: String },
    IsNull { column: String, negated: bool },
    Constant(bool),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Quoted(String),
    Text(String),
    Number(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Dot,
}

fn outside(what: String) -> EnforceError {
    EnforceError::PredicateOutsideGrammar(what)
}

fn lex(src: &str) -> Result<Vec<Token>, EnforceError> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => (out.push(Token::LParen), i += 1).1,
            ')' => (out.push(Token::RParen), i += 1).1,
            ',' => (out.push(Token::Comma), i += 1).1,
            '.' => (out.push(Token::Dot), i += 1).1,
            '\'' | '"' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(outside(format!("unterminated {quote} at `{src}`"))),
                        Some(&q) if q == quote && chars.get(i + 1) == Some(&quote) => {
                            s.push(quote);
                            i += 2;
                        }
                        Some(&q) if q == quote => {
                            i += 1;
                            break;
                        }
                        Some(&ch) => {
                            s.push(ch);
                            i += 1;
                        }
                    }
                }
                out.push(if quote == '\'' { Token::Text(s) } else { Token::Quoted(s) });
            }
            '<' | '>' | '=' | '!' => {
                let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                let op = match two.as_str() {
                    "<=" => Some("<="),
                    ">=" => Some(">="),
                    "<>" => Some("<>"),
                    _ => None,
                };
                if let Some(op) = op {
                    out.push(Token::Op(op));
                    i += 2;
                } else {
                    out.push(Token::Op(match c {
                        '<' => "<",
                        '>' => ">",
                        '=' => "=",
                        _ => return Err(outside(format!("operator `{two}` is outside the grammar"))),
                    }));
                    i += 1;
                }
            }
            c if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit)) => {
                let start = i;
                i += 1;
                while chars.get(i).is_some_and(|d| d.is_ascii_digit() || *d == '.') {
                    i += 1;
                }
                let n: String = chars[start..i].iter().collect();
                if n.parse::<f64>().is_err() {
                    return Err(outside(format!("`{n}` is not a number")));
                }
                out.push(Token::Number(n));
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let start = i;
                while chars.get(i).is_some_and(|d| d.is_ascii_alphanumeric() || *d == '_') {
                    i += 1;
                }
                out.push(Token::Word(chars[start..i].iter().collect()));
            }
            other => return Err(outside(format!("`{other}` is outside the grammar"))),
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn keyword(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Token::Word(w)) if w.eq_ignore_ascii_case(k))
    }

    fn eat_keyword(&mut self, k: &str) -> bool {
        let hit = self.keyword(k);
        if hit {
            self.at += 1;
        }
        hit
    }

    fn expect(&mut self, t: Token) -> Result<(), EnforceError> {
        if self.peek() == Some(&t) {
            self.at += 1;
            Ok(())
        } else {
            Err(outside(format!("expected {t:?}, found {:?}", self.peek())))
        }
    }

    fn disjunction(&mut self) -> Result<Predicate, EnforceError> {
        let mut parts = vec![self.conjunction()?];
        while self.eat_keyword("OR") {
            parts.push(self.conjunction()?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { Predicate::Or(parts) })
    }

    fn conjunction(&mut self) -> Result<Predicate, EnforceError> {
        let mut parts = vec![self.negation()?];
        while self.eat_keyword("AND") {
            parts.push(self.negation()?);
        }
        Ok(if parts.len() == 1 { parts.remove(0) } else { Predicate::And(parts) })
    }

    fn negation(&mut self) -> Result<Predicate, EnforceError> {
        if self.eat_keyword("NOT") {
            return Ok(Predicate::Not(Box::new(self.primary()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Predicate, EnforceError> {
        if self.peek() == Some(&Token::LParen) {
            self.at += 1;
            let p = self.disjunction()?;
            self.expect(Token::RParen)?;
            return Ok(p);
        }
        let left = self.operand()?;
        if let (Operand::Literal(Literal::Boolean(b)), false) = (&left, self.at_comparison()) {
            return Ok(Predicate::Constant(*b));
        }
        let negated_here = self.keyword("NOT");
        if self.keyword("IN") || self.keyword("LIKE") || negated_here || self.keyword("IS") {
            let column = match &left {
                Operand::Column(c) => c.clone(),
                other => return Err(outside(format!("{other:?} is not a column; membership, pattern and null tests take a column"))),
            };
            if self.eat_keyword("IS") {
                let negated = self.eat_keyword("NOT");
                if !self.eat_keyword("NULL") {
                    return Err(outside("IS takes NULL or NOT NULL".into()));
                }
                return Ok(Predicate::IsNull { column, negated });
            }
            let negated = self.eat_keyword("NOT");
            if self.eat_keyword("IN") {
                self.expect(Token::LParen)?;
                let mut list = vec![self.literal()?];
                while self.peek() == Some(&Token::Comma) {
                    self.at += 1;
                    list.push(self.literal()?);
                }
                self.expect(Token::RParen)?;
                if list.len() > MEMBERSHIP_LIST_ENTRIES {
                    return Err(outside(format!(
                        "a membership list on `{column}` holds {} entries; the bound is {MEMBERSHIP_LIST_ENTRIES}",
                        list.len()
                    )));
                }
                return Ok(Predicate::In { column, negated, list });
            }
            if self.eat_keyword("LIKE") {
                let pattern = match self.literal()? {
                    Literal::Text(s) => s,
                    other => return Err(outside(format!("LIKE takes a string, found {other:?}"))),
                };
                return Ok(Predicate::Like { column, negated, pattern });
            }
            return Err(outside("NOT after an operand takes IN or LIKE".into()));
        }
        let op = match self.peek() {
            Some(Token::Op(op)) => *op,
            other => return Err(outside(format!("expected a comparison, found {other:?}"))),
        };
        self.at += 1;
        let cmp = match op {
            "=" => Comparison::Eq,
            "<>" => Comparison::Ne,
            "<" => Comparison::Lt,
            "<=" => Comparison::Le,
            ">" => Comparison::Gt,
            ">=" => Comparison::Ge,
            _ => unreachable!("the lexer emits no other operator"),
        };
        let right = self.operand()?;
        Ok(Predicate::Compare(left, cmp, right))
    }

    fn at_comparison(&self) -> bool {
        matches!(self.peek(), Some(Token::Op(_)))
    }

    fn literal(&mut self) -> Result<Literal, EnforceError> {
        match self.operand()? {
            Operand::Literal(l) => Ok(l),
            other => Err(outside(format!("{other:?} is not a literal"))),
        }
    }

    fn operand(&mut self) -> Result<Operand, EnforceError> {
        let t = self.peek().cloned().ok_or_else(|| outside("the predicate ends where an operand belongs".into()))?;
        self.at += 1;
        match t {
            Token::Text(s) => Ok(Operand::Literal(Literal::Text(s))),
            Token::Number(n) => Ok(Operand::Literal(Literal::Number(n))),
            Token::Quoted(c) => Ok(Operand::Column(c)),
            Token::Word(w) if w.eq_ignore_ascii_case("true") => Ok(Operand::Literal(Literal::Boolean(true))),
            Token::Word(w) if w.eq_ignore_ascii_case("false") => Ok(Operand::Literal(Literal::Boolean(false))),
            Token::Word(w) if w == "subject" && self.peek() == Some(&Token::Dot) => {
                self.at += 1;
                match self.peek().cloned() {
                    Some(Token::Word(field)) if SUBJECT_FIELDS.contains(&field.as_str()) => {
                        self.at += 1;
                        Ok(Operand::Subject(field))
                    }
                    other => Err(outside(format!("`subject.{other:?}` names no subject member"))),
                }
            }
            Token::Word(w) if self.peek() == Some(&Token::LParen) => {
                let name = w.to_ascii_lowercase();
                if !SAFE_FUNCTIONS.contains(&name.as_str()) {
                    return Err(outside(format!("function `{w}` is not declared safe")));
                }
                self.at += 1;
                let mut args = Vec::new();
                if self.peek() != Some(&Token::RParen) {
                    args.push(self.operand()?);
                    while self.peek() == Some(&Token::Comma) {
                        self.at += 1;
                        args.push(self.operand()?);
                    }
                }
                self.expect(Token::RParen)?;
                Ok(Operand::Call(name, args))
            }
            Token::Word(w) if !is_keyword(&w) => Ok(Operand::Column(w)),
            other => Err(outside(format!("{other:?} is outside the grammar"))),
        }
    }
}

fn is_keyword(w: &str) -> bool {
    ["AND", "OR", "NOT", "IN", "LIKE", "IS", "NULL", "SELECT", "FROM", "WHERE"].iter().any(|k| w.eq_ignore_ascii_case(k))
}

impl Predicate {
    /// Parse a predicate at manifest load. A node outside the grammar refuses, naming it
    /// (`authority.filter-rows.outside-grammar`), as does source text past 4 KiB.
    pub fn parse(src: &str) -> Result<Predicate, EnforceError> {
        if src.len() > PREDICATE_SIZE {
            return Err(outside(format!("the predicate holds {} bytes of source; the bound is {PREDICATE_SIZE}", src.len())));
        }
        let mut p = Parser { tokens: lex(src)?, at: 0 };
        let pred = p.disjunction()?;
        if let Some(extra) = p.peek() {
            return Err(outside(format!("{extra:?} follows a complete predicate")));
        }
        Ok(pred)
    }

    /// Whether the predicate reads no column: an exception condition is over subject
    /// fields alone (`authority.filter-rows.exception`).
    pub fn subject_only(&self) -> bool {
        fn operand(o: &Operand) -> bool {
            match o {
                Operand::Column(_) => false,
                Operand::Call(_, args) => args.iter().all(operand),
                _ => true,
            }
        }
        match self {
            Predicate::Or(ps) | Predicate::And(ps) => ps.iter().all(Predicate::subject_only),
            Predicate::Not(p) => p.subject_only(),
            Predicate::Compare(l, _, r) => operand(l) && operand(r),
            Predicate::In { .. } | Predicate::Like { .. } | Predicate::IsNull { .. } => false,
            Predicate::Constant(_) => true,
        }
    }

    /// The SQL this predicate compiles to. Every identifier is double-quoted, and a
    /// subject field reads the subject relation, so a claim value never enters the text.
    pub fn sql(&self) -> String {
        match self {
            Predicate::Or(ps) => format!("({})", ps.iter().map(Predicate::sql).collect::<Vec<_>>().join(" OR ")),
            Predicate::And(ps) => format!("({})", ps.iter().map(Predicate::sql).collect::<Vec<_>>().join(" AND ")),
            Predicate::Not(p) => format!("(NOT {})", p.sql()),
            Predicate::Compare(l, op, r) => {
                let op = match op {
                    Comparison::Eq => "=",
                    Comparison::Ne => "<>",
                    Comparison::Lt => "<",
                    Comparison::Le => "<=",
                    Comparison::Gt => ">",
                    Comparison::Ge => ">=",
                };
                format!("({} {op} {})", operand_sql(l), operand_sql(r))
            }
            Predicate::In { column, negated, list } => format!(
                "({} {}IN ({}))",
                ident(column),
                if *negated { "NOT " } else { "" },
                list.iter().map(literal_sql).collect::<Vec<_>>().join(", ")
            ),
            Predicate::Like { column, negated, pattern } => {
                format!("({} {}LIKE {})", ident(column), if *negated { "NOT " } else { "" }, literal(pattern))
            }
            Predicate::IsNull { column, negated } => {
                format!("({} IS {}NULL)", ident(column), if *negated { "NOT " } else { "" })
            }
            Predicate::Constant(b) => b.to_string(),
        }
    }

    /// Columns the predicate names.
    pub fn columns(&self) -> Vec<&str> {
        fn operand<'a>(o: &'a Operand, out: &mut Vec<&'a str>) {
            match o {
                Operand::Column(c) => out.push(c),
                Operand::Call(_, args) => args.iter().for_each(|a| operand(a, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        match self {
            Predicate::Or(ps) | Predicate::And(ps) => ps.iter().for_each(|p| out.extend(p.columns())),
            Predicate::Not(p) => out.extend(p.columns()),
            Predicate::Compare(l, _, r) => {
                operand(l, &mut out);
                operand(r, &mut out);
            }
            Predicate::In { column, .. } | Predicate::Like { column, .. } | Predicate::IsNull { column, .. } => out.push(column),
            Predicate::Constant(_) => {}
        }
        out
    }
}

fn literal_sql(l: &Literal) -> String {
    match l {
        Literal::Text(s) => literal(s),
        Literal::Number(n) => n.clone(),
        Literal::Boolean(b) => b.to_string(),
    }
}

fn operand_sql(o: &Operand) -> String {
    match o {
        Operand::Column(c) => ident(c),
        Operand::Subject(f) => format!("(SELECT {} FROM {})", ident(f), ident(SUBJECT_RELATION)),
        Operand::Literal(l) => literal_sql(l),
        Operand::Call(f, args) => format!("{f}({})", args.iter().map(operand_sql).collect::<Vec<_>>().join(", ")),
    }
}
