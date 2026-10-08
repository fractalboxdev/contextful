//! Typed removal rules and pure bounded value rewriting (`authority.redact`).
use crate::enforce::EnforceError;
use regex_automata::nfa::thompson::{self, State, WhichCaptures, NFA};
use regex_automata::util::primitives::StateID;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `authority.redact.pattern-bytes`.
pub const PATTERN_BYTES: usize = 4 * 1024;
/// `authority.redact.compiled-pattern-size`.
pub const COMPILED_PATTERN_SIZE: usize = 1024 * 1024;
/// `authority.redact.rules-per-pipeline`.
pub const RULES_PER_PIPELINE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Whole {
    Whole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Matcher {
    Whole(Whole),
    Pattern(Pattern),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pattern {
    pub pattern: String,
}

impl Default for Matcher {
    fn default() -> Self {
        Self::Whole(Whole::Whole)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Drop,
    Replace,
    Hash,
    Tokenize,
    Truncate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub table: String,
    pub column: String,
    #[serde(default, rename = "match")]
    pub matcher: Matcher,
    pub operation: Operation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_path: Option<String>,
}

fn invalid(why: impl Into<String>) -> EnforceError {
    EnforceError::RedactionInvalid(why.into())
}

#[derive(Debug, Clone)]
enum PathStep {
    Key(String),
    Index(usize),
    Every,
}

/// A concrete source path emitted by normalization, rather than inferred from names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueStep {
    Key(String),
    Index(usize),
}

fn path(text: &str) -> Result<Vec<PathStep>, EnforceError> {
    let mut rest = text.strip_prefix('$').ok_or_else(|| invalid("a JSON path begins with `$`"))?;
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(next) = rest.strip_prefix('.') {
            let end = next.find(['.', '[']).unwrap_or(next.len());
            let key = &next[..end];
            if key.is_empty() || !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Err(invalid("a dotted JSON key contains letters, numbers or underscore"));
            }
            steps.push(PathStep::Key(key.into()));
            rest = &next[end..];
        } else if let Some(next) = rest.strip_prefix('[') {
            let end = next.find(']').ok_or_else(|| invalid("an array selector ends with `]`"))?;
            let step = match &next[..end] {
                "*" => PathStep::Every,
                index if !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) => {
                    PathStep::Index(index.parse().map_err(|_| invalid("an array selector exceeds its index range"))?)
                }
                _ => return Err(invalid("an array selector is a decimal index or `*`")),
            };
            steps.push(step);
            rest = &next[end + 1..];
        } else {
            return Err(invalid("unsupported JSON path syntax"));
        }
    }
    Ok(steps)
}

/// A compiled rule holds no pepper or other key material.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub rule: Rule,
    regex: Option<std::sync::Arc<NFA>>,
    path: Option<Vec<PathStep>>,
}

impl CompiledRule {
    /// Apply a parent-column rule to the concrete cell selected by normalization.
    pub fn rewrite_projected(
        &self,
        value: &mut Value,
        lineage: &[ValueStep],
        substitute: &dyn Fn(&Rule, &str) -> Result<Option<String>, EnforceError>,
    ) -> Result<(), EnforceError> {
        let Some(selected) = &self.path else { return self.cell(value, substitute) };
        for (selected, actual) in selected.iter().zip(lineage) {
            let matches = match (selected, actual) {
                (PathStep::Key(a), ValueStep::Key(b)) => a == b,
                (PathStep::Index(a), ValueStep::Index(b)) => a == b,
                (PathStep::Every, ValueStep::Index(_)) => true,
                _ => false,
            };
            if !matches {
                return Ok(());
            }
        }
        if selected.len() <= lineage.len() {
            self.cell(value, substitute)
        } else {
            self.addressed(value, &selected[lineage.len()..], substitute)
        }
    }
    pub fn compile(rule: Rule) -> Result<Self, EnforceError> {
        if rule.table.is_empty() || rule.column.is_empty() || rule.column.starts_with('_') {
            return Err(invalid("a removal rule names a table and a non-reserved column"));
        }
        match (rule.operation, &rule.argument) {
            (Operation::Replace, Some(Value::String(class))) if !class.is_empty() => {}
            (Operation::Truncate, Some(n)) if n.as_u64().is_some_and(|n| n > 0 && n <= u32::MAX as u64) => {}
            (Operation::Drop | Operation::Hash | Operation::Tokenize, None) => {}
            _ => return Err(invalid("replace takes a class; truncate takes a positive character count; other operations take no argument")),
        }
        let regex = match &rule.matcher {
            Matcher::Whole(_) => None,
            Matcher::Pattern(pattern) => {
                if pattern.pattern.len() > PATTERN_BYTES {
                    return Err(invalid("pattern source exceeds its byte bound"));
                }
                let hir = regex_syntax::Parser::new().parse(&pattern.pattern).map_err(|e| invalid(e.to_string()))?;
                if hir.properties().minimum_len().is_none_or(|n| n == 0) {
                    return Err(invalid("a removal pattern must match at least one byte"));
                }
                if hir.properties().look_set().contains_word_unicode() {
                    return Err(invalid("a removal pattern uses ASCII word boundaries alone"));
                }
                Some(std::sync::Arc::new(
                    thompson::Compiler::new()
                        .configure(thompson::Config::new().nfa_size_limit(Some(COMPILED_PATTERN_SIZE)).which_captures(WhichCaptures::None))
                        .build(&pattern.pattern)
                        .map_err(|e| invalid(e.to_string()))?,
                ))
            }
        };
        let selected = rule.json_path.as_deref().map(path).transpose()?;
        Ok(Self { rule, regex, path: selected })
    }

    /// Rewrite addressed values using a policy-owned substitute over every matched span.
    pub fn rewrite(&self, value: &mut Value, substitute: &dyn Fn(&Rule, &str) -> Result<Option<String>, EnforceError>) -> Result<(), EnforceError> {
        if value.is_null() {
            return Ok(());
        }
        match &self.path {
            None => self.cell(value, substitute),
            Some(steps) => self.addressed(value, steps, substitute),
        }
    }

    fn addressed(
        &self,
        value: &mut Value,
        steps: &[PathStep],
        substitute: &dyn Fn(&Rule, &str) -> Result<Option<String>, EnforceError>,
    ) -> Result<(), EnforceError> {
        if value.is_null() {
            return Ok(());
        }
        if let Value::String(text) = value {
            let mut json: Value = serde_json::from_str(text).map_err(|_| invalid("a path-selected cell contains invalid JSON"))?;
            self.visit(&mut json, steps, substitute)?;
            *text = crate::pipeline::canonical::canonical_json(&json);
            Ok(())
        } else {
            self.visit(value, steps, substitute)
        }
    }

    fn visit(
        &self,
        value: &mut Value,
        steps: &[PathStep],
        substitute: &dyn Fn(&Rule, &str) -> Result<Option<String>, EnforceError>,
    ) -> Result<(), EnforceError> {
        let Some((step, rest)) = steps.split_first() else { return self.cell(value, substitute) };
        match step {
            PathStep::Key(key) => {
                if let Some(v) = value.as_object_mut().and_then(|o| o.get_mut(key)) {
                    self.visit(v, rest, substitute)?;
                }
            }
            PathStep::Index(index) => {
                if let Some(v) = value.as_array_mut().and_then(|a| a.get_mut(*index)) {
                    self.visit(v, rest, substitute)?;
                }
            }
            PathStep::Every => {
                if let Some(items) = value.as_array_mut() {
                    for v in items {
                        self.visit(v, rest, substitute)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn cell(&self, value: &mut Value, substitute: &dyn Fn(&Rule, &str) -> Result<Option<String>, EnforceError>) -> Result<(), EnforceError> {
        if value.is_null() {
            return Ok(());
        }
        if let Some(regex) = &self.regex {
            let text = value.as_str().ok_or_else(|| invalid("pattern removal selects text values alone"))?;
            let mut out = String::new();
            let mut cursor = 0;
            for (start, end) in spans(regex, text)? {
                out.push_str(&text[cursor..start]);
                if self.rule.operation != Operation::Drop {
                    if let Some(replacement) = substitute(&self.rule, &text[start..end])? {
                        out.push_str(&replacement);
                    }
                }
                cursor = end;
            }
            out.push_str(&text[cursor..]);
            *value = Value::String(out);
        } else if self.rule.operation == Operation::Drop {
            *value = Value::Null;
        } else {
            let text = value.as_str().map(str::to_string).unwrap_or_else(|| crate::pipeline::canonical::canonical_json(value));
            *value = substitute(&self.rule, &text)?.map(Value::String).unwrap_or(Value::Null);
        }
        Ok(())
    }
}

/// The union of every match, as non-overlapping spans: overlapping matches merge and
/// adjacent ones stay separate. One forward pass keeps, per NFA state, the leftmost start
/// that reaches it, so each end yields the widest match ending there and the work is
/// linear in the value's length with no restart.
fn spans(nfa: &NFA, text: &str) -> Result<Vec<(usize, usize)>, EnforceError> {
    let bytes = text.as_bytes();
    let mut seen = vec![usize::MAX; nfa.states().len()];
    let (mut current, mut next, mut stack) = (Vec::new(), Vec::new(), Vec::new());
    let mut found: Vec<(usize, usize)> = Vec::new();
    for at in 0..=bytes.len() {
        // Threads arrive in ascending start order, so the first visit to a state holds its leftmost start.
        next.clear();
        for &(state, start) in &current {
            let target = match nfa.state(state) {
                State::ByteRange { trans } => trans.matches_byte(bytes[at - 1]).then_some(trans.next),
                State::Sparse(sparse) => sparse.matches_byte(bytes[at - 1]),
                State::Dense(dense) => dense.matches_byte(bytes[at - 1]),
                _ => None,
            };
            if let Some(target) = target {
                close(nfa, bytes, at, target, start, &mut seen, &mut stack, &mut next);
            }
        }
        close(nfa, bytes, at, nfa.start_anchored(), at, &mut seen, &mut stack, &mut next);
        if let Some(&(_, start)) = next.iter().find(|(state, _)| matches!(nfa.state(*state), State::Match { .. })) {
            let mut start = start;
            while let Some(&(previous, end)) = found.last() {
                if end <= start {
                    break;
                }
                start = start.min(previous);
                found.pop();
            }
            if !text.is_char_boundary(start) || !text.is_char_boundary(at) {
                return Err(invalid("a removal pattern matched inside a UTF-8 character"));
            }
            found.push((start, at));
        }
        std::mem::swap(&mut current, &mut next);
    }
    Ok(found)
}

/// Add the epsilon closure of `state` at `at`, skipping states an earlier start reached.
#[allow(clippy::too_many_arguments)]
fn close(nfa: &NFA, bytes: &[u8], at: usize, state: StateID, start: usize, seen: &mut [usize], stack: &mut Vec<StateID>, set: &mut Vec<(StateID, usize)>) {
    stack.push(state);
    while let Some(state) = stack.pop() {
        if seen[state.as_usize()] == at {
            continue;
        }
        seen[state.as_usize()] = at;
        match nfa.state(state) {
            State::ByteRange { .. } | State::Sparse(_) | State::Dense(_) | State::Match { .. } => set.push((state, start)),
            State::Look { look, next } => {
                if nfa.look_matcher().matches(*look, bytes, at) {
                    stack.push(*next);
                }
            }
            State::Union { alternates } => stack.extend(alternates.iter().rev()),
            State::BinaryUnion { alt1, alt2 } => stack.extend([*alt2, *alt1]),
            State::Capture { next, .. } => stack.push(*next),
            State::Fail => {}
        }
    }
}
