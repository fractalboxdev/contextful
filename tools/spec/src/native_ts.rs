//! Syntax shared by the spec checker and the TypeScript surface gate.

pub struct TestCall<'a> {
    pub title: &'a str,
    pub disabled: bool,
}

pub fn tag(line: &str) -> Option<(&str, &str)> {
    let text = line.trim().strip_prefix("//")?.trim_start().strip_prefix("spec:")?.trim_start();
    let (id, rev) = text.split_once('@')?;
    if id.is_empty() || id.chars().any(char::is_whitespace) || rev.chars().any(char::is_whitespace) {
        return None;
    }
    Some((id, rev))
}

pub fn test_call(line: &str) -> Option<TestCall<'_>> {
    let line = line.trim();
    let rest = line.strip_prefix("test").or_else(|| line.strip_prefix("it"))?;
    let (disabled, rest) = if let Some(rest) = rest.strip_prefix(".skip").or_else(|| rest.strip_prefix(".todo")).or_else(|| rest.strip_prefix(".only")) {
        (true, rest)
    } else {
        (false, rest)
    };
    let rest = rest.strip_prefix('(')?.trim_start();
    let quote = rest.chars().next().filter(|ch| matches!(ch, '\'' | '"' | '`'))?;
    let end = rest[1..].find(quote)?;
    Some(TestCall { title: &rest[1..end + 1], disabled })
}
