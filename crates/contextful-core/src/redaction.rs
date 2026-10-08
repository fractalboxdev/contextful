//! Typed removal rules and pure bounded value rewriting (`authority.redact`).
use crate::enforce::EnforceError;
use regex_automata::{hybrid, nfa::thompson, Anchored, Input};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `authority.redact.pattern-bytes`.
pub const PATTERN_BYTES: usize = 4 * 1024;
/// `authority.redact.compiled-pattern-size`.
pub const COMPILED_PATTERN_SIZE: usize = 1024 * 1024;
/// `authority.redact.rules-per-pipeline`.
pub const RULES_PER_PIPELINE: usize = 256;
/// `authority.redact.match-work`.
pub const MATCH_STEPS_PER_BYTE: usize = 16;

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
    regex: Option<std::sync::Arc<hybrid::regex::Regex>>,
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
                    hybrid::regex::Regex::builder()
                        .thompson(thompson::Config::new().nfa_size_limit(Some(COMPILED_PATTERN_SIZE)))
                        .build(&pattern.pattern)
                        .map_err(|e| invalid(e.to_string()))?,
                ))
            }
        };
        let selected = rule.json_path.as_deref().map(path).transpose()?;
        Ok(Self { rule, regex, path: selected })
    }

    /// Rewrite addressed values using a policy-owned substitute over leftmost-first spans.
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

/// Leftmost-first non-overlapping spans. Each forward automaton step draws on one budget
/// of `MATCH_STEPS_PER_BYTE` per byte, so an adversarial value refuses its write instead
/// of either scanning quadratically or leaving the tail of a match intact.
fn spans(regex: &hybrid::regex::Regex, text: &str) -> Result<Vec<(usize, usize)>, EnforceError> {
    let exhausted = || invalid("a removal pattern exceeds its matching work bound on one value");
    let (forward, reverse) = (regex.forward(), regex.reverse());
    let mut cache = regex.create_cache();
    let (forward_cache, reverse_cache) = cache.as_parts_mut();
    let bytes = text.as_bytes();
    let mut budget = bytes.len().saturating_mul(MATCH_STEPS_PER_BYTE);
    let mut found = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let input = Input::new(text).span(cursor..bytes.len());
        let mut state = forward.start_state_forward(forward_cache, &input).map_err(|_| exhausted())?;
        let mut end = None;
        let mut at = cursor;
        loop {
            if at == bytes.len() {
                state = forward.next_eoi_state(forward_cache, state).map_err(|_| exhausted())?;
                if state.is_match() {
                    end = Some(at);
                }
                break;
            }
            budget = budget.checked_sub(1).ok_or_else(exhausted)?;
            state = forward.next_state(forward_cache, state, bytes[at]).map_err(|_| exhausted())?;
            if state.is_tagged() {
                if state.is_match() {
                    end = Some(at);
                } else if state.is_dead() {
                    break;
                } else if state.is_quit() {
                    return Err(exhausted());
                }
            }
            at += 1;
        }
        let Some(end) = end else { break };
        let back = Input::new(text).span(cursor..end).anchored(Anchored::Yes);
        let start = reverse.try_search_rev(reverse_cache, &back).map_err(|_| exhausted())?.ok_or_else(exhausted)?.offset();
        found.push((start, end));
        cursor = end;
    }
    Ok(found)
}
