use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};

use crate::config::{Match, TriggerMode, UppercaseStyle, VariableKind};

#[derive(Debug, PartialEq)]
pub struct Expansion {
    pub delete_count: usize,
    pub text: String,
    pub cursor_back: usize,
    pub undo_text: String,
}

pub struct Expander {
    buffer: VecDeque<char>,
    max_trigger_len: usize,
    matches: Vec<CompiledMatch>,
    trigger_mode: TriggerMode,
    terminators: Vec<char>,
    word_separators: Option<Vec<char>>,
    pending_prompt: Option<DeferredMatch>,
}

#[derive(Clone)]
pub(crate) struct CompiledMatch {
    trigger: String,
    regex: Option<regex::Regex>,
    source: std::path::PathBuf,
    ambiguous: bool,
    replace: String,
    vars: Vec<crate::config::Variable>,
    fields: Vec<crate::fields::Field>,
    left_word: bool,
    right_word: bool,
    propagate_case: bool,
    uppercase_style: UppercaseStyle,
}

pub(crate) trait IntoCompiledMatches {
    fn append_compiled(self, output: &mut Vec<CompiledMatch>);
}

impl IntoCompiledMatches for Match {
    fn append_compiled(self, output: &mut Vec<CompiledMatch>) {
        for trigger in &self.triggers {
            output.push(CompiledMatch {
                trigger: trigger.clone(),
                regex: None,
                source: self.source.clone(),
                ambiguous: false,
                replace: self.replace.clone(),
                vars: self.vars.clone(),
                fields: self.fields.clone(),
                left_word: self.word || self.left_word,
                right_word: self.word || self.right_word,
                propagate_case: self.propagate_case,
                uppercase_style: self.uppercase_style,
            });
        }
        if let Some(pattern) = &self.regex {
            output.push(CompiledMatch {
                trigger: String::new(),
                regex: Some(
                    regex::Regex::new(&format!("(?:{pattern})$"))
                        .expect("regex triggers were validated"),
                ),
                source: self.source,
                ambiguous: false,
                replace: self.replace,
                vars: self.vars,
                fields: self.fields,
                left_word: self.word || self.left_word,
                right_word: self.word || self.right_word,
                propagate_case: self.propagate_case,
                uppercase_style: self.uppercase_style,
            });
        }
    }
}

impl IntoCompiledMatches for (String, String) {
    fn append_compiled(self, output: &mut Vec<CompiledMatch>) {
        output.push(CompiledMatch {
            trigger: self.0,
            regex: None,
            source: std::path::PathBuf::new(),
            ambiguous: false,
            replace: self.1,
            vars: Vec::new(),
            fields: Vec::new(),
            left_word: false,
            right_word: false,
            propagate_case: false,
            uppercase_style: UppercaseStyle::Uppercase,
        });
    }
}

impl Expander {
    #[cfg(test)]
    pub fn new<T: IntoCompiledMatches>(matches: Vec<T>, trigger_mode: TriggerMode) -> Self {
        Self::new_configured(matches, trigger_mode, vec![' '], None, 256)
    }

    pub fn new_configured<T: IntoCompiledMatches>(
        matches: Vec<T>,
        trigger_mode: TriggerMode,
        terminators: Vec<char>,
        word_separators: Option<Vec<char>>,
        regex_max_buffer: usize,
    ) -> Self {
        let matches = compile_matches(matches);
        let mut max_trigger_len = matches
            .iter()
            .map(|item| item.trigger.chars().count())
            .max()
            .unwrap_or(0);
        if matches.iter().any(|item| item.regex.is_some()) {
            max_trigger_len = max_trigger_len.max(regex_max_buffer);
        }
        Self {
            buffer: VecDeque::new(),
            max_trigger_len,
            matches,
            trigger_mode,
            terminators,
            word_separators,
            pending_prompt: None,
        }
    }

    #[cfg(test)]
    pub fn update<T: IntoCompiledMatches>(&mut self, matches: Vec<T>, trigger_mode: TriggerMode) {
        self.update_configured(matches, trigger_mode, vec![' '], None, 256);
    }

    pub fn update_configured<T: IntoCompiledMatches>(
        &mut self,
        matches: Vec<T>,
        trigger_mode: TriggerMode,
        terminators: Vec<char>,
        word_separators: Option<Vec<char>>,
        regex_max_buffer: usize,
    ) {
        let matches = compile_matches(matches);
        self.max_trigger_len = matches
            .iter()
            .map(|item| item.trigger.chars().count())
            .max()
            .unwrap_or(0);
        if matches.iter().any(|item| item.regex.is_some()) {
            self.max_trigger_len = self.max_trigger_len.max(regex_max_buffer);
        }
        self.pending_prompt = None;
        self.matches = matches;
        self.trigger_mode = trigger_mode;
        self.terminators = terminators;
        self.word_separators = word_separators;
        self.buffer.clear();
    }

    pub fn push_char(&mut self, c: char) -> Option<Expansion> {
        if self.max_trigger_len == 0 {
            return None;
        }
        self.pending_prompt = None;
        if c == ' ' && !(self.trigger_mode == TriggerMode::Space && self.terminators.contains(&c)) {
            let had_input = !self.buffer.is_empty();
            let expansion = self.find_match(Some(c));
            if expansion.is_some()
                || self.pending_prompt.is_some()
                || (had_input && self.buffer.is_empty())
            {
                self.buffer.clear();
                return expansion;
            }
        }
        if self.trigger_mode == TriggerMode::Space && self.terminators.contains(&c) {
            let expansion = self.find_match(Some(c));
            self.buffer.clear();
            return expansion;
        }

        self.buffer.push_back(c);

        // Keep a left boundary and a possible trailing separator for word checks.
        while self.buffer.len() > self.max_trigger_len + 2 {
            self.buffer.pop_front();
        }

        if self.trigger_mode == TriggerMode::Space {
            return None;
        }

        if self.is_word_separator(c) {
            if let Some(expansion) = self.find_right_word_match(c) {
                return Some(expansion);
            }
        }

        self.find_match(None)
    }

    fn is_word_separator(&self, value: char) -> bool {
        self.word_separators
            .as_ref()
            .map_or_else(|| !is_word_char(value), |values| values.contains(&value))
    }

    fn find_match(&mut self, terminator: Option<char>) -> Option<Expansion> {
        let buf_str: String = self.buffer.iter().collect();
        let terminator_count = usize::from(terminator.is_some());

        for item in &self.matches {
            if item.ambiguous || (!item.fields.is_empty() && terminator != Some(' ')) {
                continue;
            }
            if self.trigger_mode == TriggerMode::Space
                && item.fields.is_empty()
                && terminator.is_some_and(|c| !self.terminators.contains(&c))
            {
                continue;
            }
            if self.trigger_mode == TriggerMode::Immediate
                && terminator.is_some()
                && item.fields.is_empty()
                && (!item.right_word || terminator.is_some_and(|c| !self.is_word_separator(c)))
            {
                continue;
            }
            if terminator_count == 0 && item.right_word {
                continue;
            }
            if let Some((typed_trigger, captures)) = matching_suffix(&buf_str, item) {
                if !left_boundary_matches(
                    &buf_str,
                    item,
                    typed_trigger.chars().count(),
                    self.word_separators.as_deref(),
                ) {
                    continue;
                }
                let delete_count = typed_trigger.chars().count() + terminator_count;
                if !item.fields.is_empty() {
                    self.pending_prompt = Some(DeferredMatch {
                        fields: item.fields.clone(),
                        original: format!("{typed_trigger} "),
                        delete_count,
                        item: item.clone(),
                        matches: self.matches.clone(),
                        captures,
                        typed_trigger,
                    });
                    self.buffer.clear();
                    return None;
                }
                let rendered = apply_propagated_case(
                    match render(&self.matches, item, &captures, chrono::Utc::now()) {
                        Ok(text) => text,
                        Err(error) => {
                            tracing::warn!("Snippet rendering failed: {error:#}");
                            self.buffer.clear();
                            return None;
                        }
                    },
                    item,
                    &typed_trigger,
                );
                let rendered = if self.trigger_mode == TriggerMode::Immediate {
                    terminator.map_or(rendered.clone(), |c| format!("{rendered}{c}"))
                } else {
                    rendered
                };
                let (text, cursor_back) = prepare_replacement(&rendered);
                self.buffer.clear();
                return Some(Expansion {
                    delete_count,
                    text,
                    cursor_back,
                    undo_text: terminator.map_or(typed_trigger.clone(), |value| {
                        format!("{typed_trigger}{value}")
                    }),
                });
            }
        }

        None
    }

    fn find_right_word_match(&mut self, separator: char) -> Option<Expansion> {
        let mut buf_str: String = self.buffer.iter().collect();
        buf_str.pop();
        for item in &self.matches {
            if item.ambiguous || !item.fields.is_empty() {
                continue;
            }
            if !item.right_word {
                continue;
            }
            let Some((typed_trigger, captures)) = matching_suffix(&buf_str, item) else {
                continue;
            };
            if !left_boundary_matches(
                &buf_str,
                item,
                typed_trigger.chars().count(),
                self.word_separators.as_deref(),
            ) {
                continue;
            }
            let rendered = apply_propagated_case(
                match render(&self.matches, item, &captures, chrono::Utc::now()) {
                    Ok(text) => text,
                    Err(error) => {
                        tracing::warn!("Snippet rendering failed: {error:#}");
                        self.buffer.clear();
                        return None;
                    }
                },
                item,
                &typed_trigger,
            );
            let replacement = format!("{}{}", rendered, separator);
            let (text, cursor_back) = prepare_replacement(&replacement);
            self.buffer.clear();
            return Some(Expansion {
                delete_count: typed_trigger.chars().count() + 1,
                text,
                cursor_back,
                undo_text: format!("{}{}", typed_trigger, separator),
            });
        }
        None
    }

    pub fn take_prompt(&mut self) -> Option<DeferredMatch> {
        self.pending_prompt.take()
    }

    pub fn pop_char(&mut self) {
        self.buffer.pop_back();
    }

    pub fn reset(&mut self) {
        self.pending_prompt = None;
        self.buffer.clear();
    }

    pub fn expansion_for_trigger(
        &self,
        trigger: &str,
        source: Option<&str>,
    ) -> Result<Option<Expansion>> {
        let Some(item) = self.matches.iter().find(|item| {
            item.regex.is_none()
                && item.trigger == trigger
                && source.is_none_or(|source| item.source.to_string_lossy() == source)
        }) else {
            return Ok(None);
        };
        if !item.fields.is_empty() {
            anyhow::bail!("requires_prompt: use physical Space with a registered prompt handler");
        }
        let (text, cursor_back) = prepare_replacement(&render(
            &self.matches,
            item,
            &HashMap::new(),
            chrono::Utc::now(),
        )?);
        Ok(Some(Expansion {
            delete_count: 0,
            text,
            cursor_back,
            undo_text: String::new(),
        }))
    }

    pub fn trigger_is_ambiguous(&self, trigger: &str) -> bool {
        self.matches
            .iter()
            .filter(|item| item.regex.is_none() && item.trigger == trigger)
            .count()
            > 1
    }
}

fn compile_matches<T: IntoCompiledMatches>(matches: Vec<T>) -> Vec<CompiledMatch> {
    let mut compiled = Vec::new();
    for item in matches {
        item.append_compiled(&mut compiled);
    }
    let mut causes = HashMap::<(bool, String), usize>::new();
    let mut folded = HashMap::<String, (usize, bool)>::new();
    for item in &compiled {
        let cause = item
            .regex
            .as_ref()
            .map_or_else(|| item.trigger.clone(), |regex| regex.as_str().to_string());
        *causes.entry((item.regex.is_some(), cause)).or_default() += 1;
        if item.regex.is_none() {
            let entry = folded.entry(item.trigger.to_lowercase()).or_default();
            entry.0 += 1;
            entry.1 |= item.propagate_case;
        }
    }
    for item in &mut compiled {
        let cause = item
            .regex
            .as_ref()
            .map_or_else(|| item.trigger.clone(), |regex| regex.as_str().to_string());
        item.ambiguous = causes[&(item.regex.is_some(), cause)] > 1
            || (item.regex.is_none()
                && folded
                    .get(&item.trigger.to_lowercase())
                    .is_some_and(|(count, insensitive)| *count > 1 && *insensitive));
    }
    compiled.sort_by_key(|item| std::cmp::Reverse(item.trigger.chars().count()));
    compiled
}

fn left_boundary_matches(
    buffer: &str,
    item: &CompiledMatch,
    matched_len: usize,
    word_separators: Option<&[char]>,
) -> bool {
    if !item.left_word {
        return true;
    }
    buffer.chars().rev().nth(matched_len).is_none_or(|value| {
        word_separators.map_or_else(|| !is_word_char(value), |values| values.contains(&value))
    })
}

fn matching_suffix(
    buffer: &str,
    item: &CompiledMatch,
) -> Option<(String, HashMap<String, String>)> {
    if let Some(regex) = &item.regex {
        let captures = regex.captures(buffer)?;
        let matched = captures.get(0)?.as_str().to_string();
        let values = regex
            .capture_names()
            .flatten()
            .map(|name| {
                (
                    name.to_string(),
                    captures
                        .name(name)
                        .map_or("", |value| value.as_str())
                        .to_string(),
                )
            })
            .collect();
        return Some((matched, values));
    }
    let trigger_len = item.trigger.chars().count();
    let suffix: String = buffer
        .chars()
        .rev()
        .take(trigger_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if suffix == item.trigger
        || (item.propagate_case && suffix.to_lowercase() == item.trigger.to_lowercase())
    {
        Some((suffix, HashMap::new()))
    } else {
        None
    }
}

fn apply_propagated_case(replacement: String, item: &CompiledMatch, typed_trigger: &str) -> String {
    if !item.propagate_case {
        return replacement;
    }
    let mut alphabetic = typed_trigger.chars().filter(|value| value.is_alphabetic());
    let Some(first) = alphabetic.next() else {
        return replacement;
    };
    let second = alphabetic.next();
    if !first.is_uppercase() {
        return replacement;
    }
    if second.is_some_and(char::is_uppercase) {
        return replacement.to_uppercase();
    }
    match item.uppercase_style {
        UppercaseStyle::CapitalizeWords => capitalize_words(&replacement),
        UppercaseStyle::Uppercase | UppercaseStyle::Capitalize => capitalize(&replacement),
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn capitalize_words(value: &str) -> String {
    let mut at_word_start = true;
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_alphabetic() && at_word_start {
            output.extend(character.to_uppercase());
            at_word_start = false;
        } else {
            output.push(character);
            at_word_start = !(character.is_alphanumeric() || character == '_');
        }
    }
    output
}

fn is_word_char(value: char) -> bool {
    value.is_alphanumeric() || value == '_'
}

fn render(
    matches: &[CompiledMatch],
    item: &CompiledMatch,
    captures: &HashMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    render_bounded(
        matches,
        item,
        captures,
        now,
        &mut crate::template::Budget::default(),
        1,
    )
}

fn render_bounded(
    matches: &[CompiledMatch],
    item: &CompiledMatch,
    captures: &HashMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
    budget: &mut crate::template::Budget,
    depth: usize,
) -> Result<String> {
    if depth > crate::template::MAX_DEPTH {
        anyhow::bail!("nested match depth exceeds {}", crate::template::MAX_DEPTH);
    }
    budget.evaluate()?;
    let capture_names: Vec<_> = captures.keys().map(String::as_str).collect();
    let order = crate::template::variable_order(&item.vars, &capture_names)?;
    let mut values = captures.clone();
    for index in order {
        budget.evaluate()?;
        let variable = &item.vars[index];
        let value = match variable.kind {
            VariableKind::Date => {
                let value = crate::date::render(&variable.params, now)?;
                budget.charge(value.len())?;
                Ok(value)
            }
            VariableKind::Echo => {
                let text = variable
                    .params
                    .echo
                    .as_deref()
                    .context("echo variable requires params.echo")?;
                if variable.inject_vars {
                    budget.interpolate(text, &values, true)
                } else {
                    budget.charge(text.len())?;
                    Ok(text.to_string())
                }
            }
            VariableKind::Match => {
                let mut candidates = matches.iter().filter(|candidate| {
                    candidate.regex.is_none() && candidate.trigger == variable.params.trigger()
                });
                let candidate = candidates
                    .next()
                    .context("nested match reference is missing")?;
                if candidates.next().is_some() {
                    anyhow::bail!("nested match reference is ambiguous");
                }
                render_bounded(matches, candidate, &HashMap::new(), now, budget, depth + 1)
            }
        }
        .with_context(|| format!("{}: variable '{}'", item.source.display(), variable.name))?;
        values.insert(variable.name.clone(), value);
    }
    budget.interpolate(&item.replace, &values, false)
}

/// An immutable candidate; values are supplied only after protocol validation.
pub struct DeferredMatch {
    pub fields: Vec<crate::fields::Field>,
    pub original: String,
    pub delete_count: usize,
    item: CompiledMatch,
    matches: Vec<CompiledMatch>,
    captures: HashMap<String, String>,
    typed_trigger: String,
}

#[derive(Clone)]
enum Segment {
    Authored(String),
    Literal(String),
    Cursor,
}

fn authored(text: &str) -> Vec<Segment> {
    let mut result = Vec::new();
    for (i, piece) in text.split("$|$").enumerate() {
        if i > 0 {
            result.push(Segment::Cursor);
        }
        result.push(Segment::Authored(piece.to_string()));
    }
    result
}
fn interpolate_segments(
    text: &str,
    values: &HashMap<String, Vec<Segment>>,
    strict: bool,
    budget: &mut crate::template::Budget,
) -> Result<Vec<Segment>> {
    let mut result = Vec::new();
    for part in crate::template::parts(text) {
        match part {
            crate::template::Part::Text(text) => {
                budget.charge(text.len())?;
                result.extend(authored(text));
            }
            crate::template::Part::Reference(name) => {
                if let Some(value) = values.get(name) {
                    for segment in value {
                        budget.charge(match segment {
                            Segment::Authored(v) | Segment::Literal(v) => v.len(),
                            Segment::Cursor => 0,
                        })?;
                    }
                    result.extend(value.clone());
                } else if strict {
                    anyhow::bail!("unknown echo reference");
                } else {
                    let text = format!("{{{{{name}}}}}");
                    budget.charge(text.len())?;
                    result.extend(authored(&text));
                }
            }
        }
    }
    Ok(result)
}
fn render_segments(
    matches: &[CompiledMatch],
    item: &CompiledMatch,
    inputs: &HashMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
    budget: &mut crate::template::Budget,
    depth: usize,
) -> Result<Vec<Segment>> {
    if depth > crate::template::MAX_DEPTH {
        anyhow::bail!("nested match depth exceeded");
    }
    budget.evaluate()?;
    let names = inputs.keys().map(String::as_str).collect::<Vec<_>>();
    let order = crate::template::variable_order(&item.vars, &names)?;
    let mut values = inputs
        .iter()
        .map(|(k, v)| (k.clone(), vec![Segment::Literal(v.clone())]))
        .collect::<HashMap<_, _>>();
    for index in order {
        budget.evaluate()?;
        let variable = &item.vars[index];
        let value = match variable.kind {
            VariableKind::Date => {
                let value = crate::date::render(&variable.params, now)?;
                budget.charge(value.len())?;
                authored(&value)
            }
            VariableKind::Echo => {
                let text = variable
                    .params
                    .echo
                    .as_deref()
                    .context("missing echo parameter")?;
                if variable.inject_vars {
                    interpolate_segments(text, &values, true, budget)?
                } else {
                    budget.charge(text.len())?;
                    authored(text)
                }
            }
            VariableKind::Match => {
                let mut candidates = matches
                    .iter()
                    .filter(|c| c.regex.is_none() && c.trigger == variable.params.trigger());
                let candidate = candidates.next().context("missing nested match")?;
                if candidates.next().is_some() || !candidate.fields.is_empty() {
                    anyhow::bail!("invalid nested match");
                }
                render_segments(matches, candidate, &HashMap::new(), now, budget, depth + 1)?
            }
        };
        values.insert(variable.name.clone(), value);
    }
    interpolate_segments(&item.replace, &values, false, budget)
}
impl DeferredMatch {
    pub fn render(&self, answers: &HashMap<String, String>) -> Result<Expansion> {
        if answers.len() != self.fields.len()
            || self.fields.iter().any(|f| !answers.contains_key(&f.id))
        {
            anyhow::bail!("missing or unexpected field answers");
        }
        let mut inputs = self.captures.clone();
        inputs.extend(answers.clone());
        let segments = render_segments(
            &self.matches,
            &self.item,
            &inputs,
            chrono::Utc::now(),
            &mut crate::template::Budget::default(),
            1,
        )?;
        let mut letters = self.typed_trigger.chars().filter(|c| c.is_alphabetic());
        let capitalize = self.item.propagate_case && letters.next().is_some_and(char::is_uppercase);
        let uppercase = capitalize && letters.next().is_some_and(char::is_uppercase);
        let words = self.item.uppercase_style == UppercaseStyle::CapitalizeWords;
        let mut first = true;
        let mut word_start = true;
        let mut text = String::new();
        let mut marker = None;
        for segment in segments {
            let literal = matches!(&segment, Segment::Literal(_));
            match segment {
                Segment::Cursor => {
                    if marker.is_some() {
                        anyhow::bail!("multiple authored cursor stops are unsupported");
                    }
                    marker = Some(text.chars().count());
                }
                Segment::Authored(value) | Segment::Literal(value) => {
                    // Literal segments advance capitalization state without changing answer bytes.
                    for character in value.chars() {
                        if !literal
                            && capitalize
                            && (uppercase
                                || (words && word_start && character.is_alphabetic())
                                || (!words && first))
                        {
                            text.extend(character.to_uppercase());
                        } else {
                            text.push(character);
                        }
                        first = false;
                        word_start = !(character.is_alphanumeric() || character == '_');
                    }
                }
            }
        }
        text.push(' ');
        // Keyboard Left movement is reliable for a single ASCII line only.
        let cursor_back = if text.is_ascii() && !text.contains(['\n', '\r', '\t']) {
            marker.map_or(0, |position| text.chars().count() - position)
        } else {
            0
        };
        Ok(Expansion {
            delete_count: self.delete_count,
            text,
            cursor_back,
            undo_text: self.original.clone(),
        })
    }
}

fn prepare_replacement(replacement: &str) -> (String, usize) {
    let Some(marker) = replacement.find("$|$") else {
        return (replacement.to_string(), 0);
    };
    let after = &replacement[marker + 3..];
    let mut text = String::with_capacity(replacement.len() - 3);
    text.push_str(&replacement[..marker]);
    text.push_str(after);
    (text, after.chars().count())
}

#[cfg(test)]
#[path = "expander_properties.rs"]
mod properties;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    pub(super) fn structured_match(trigger: &str, replace: &str) -> Match {
        Match {
            triggers: vec![trigger.to_string()],
            regex: None,
            label: None,
            search_terms: Vec::new(),
            replace: replace.to_string(),
            vars: Vec::new(),
            fields: Vec::new(),
            word: false,
            left_word: false,
            right_word: false,
            propagate_case: false,
            uppercase_style: UppercaseStyle::Uppercase,
            source: PathBuf::from("test.yml"),
        }
    }

    fn prompted(trigger: &str, replacement: &str) -> Match {
        let mut item = structured_match(trigger, replacement);
        item.fields = vec![crate::fields::Field {
            id: "value".into(),
            label: "Value".into(),
            kind: crate::fields::FieldKind::Text,
            default: None,
            options: Vec::new(),
        }];
        item
    }

    #[test]
    fn prompts_wait_for_space_in_both_modes_and_answers_remain_literal() {
        for mode in [TriggerMode::Immediate, TriggerMode::Space] {
            let mut e = Expander::new(
                vec![prompted(";form", "before {{value}} {{value}}$|$end")],
                mode,
            );
            for c in ";form".chars() {
                assert!(e.push_char(c).is_none());
                assert!(e.take_prompt().is_none());
            }
            assert!(e.push_char(' ').is_none());
            let prompt = e.take_prompt().unwrap();
            assert_eq!(prompt.original, ";form ");
            let answer = "{{other}}$|$";
            let expansion = prompt
                .render(&HashMap::from([("value".into(), answer.into())]))
                .unwrap();
            assert_eq!(expansion.text, "before {{other}}$|$ {{other}}$|$end ");
            assert_eq!(expansion.cursor_back, 4);
            assert_eq!(expansion.delete_count, 6);
        }
    }

    #[test]
    fn prompt_literal_provenance_survives_echo_and_capitalization() {
        let mut item = prompted(";form", "{{copy}} hello$|$");
        item.propagate_case = true;
        item.vars = vec![crate::config::Variable {
            name: "copy".into(),
            kind: VariableKind::Echo,
            inject_vars: true,
            params: crate::config::VariableParams {
                echo: Some("{{value}}".into()),
                ..Default::default()
            },
        }];
        let mut e = Expander::new(vec![item], TriggerMode::Immediate);
        for c in ";FORM ".chars() {
            assert!(e.push_char(c).is_none());
        }
        let expansion = e
            .take_prompt()
            .unwrap()
            .render(&HashMap::from([("value".into(), "é\n{{copy}}$|$".into())]))
            .unwrap();
        assert_eq!(expansion.text, "é\n{{copy}}$|$ HELLO ");
        assert_eq!(expansion.cursor_back, 0);
    }

    #[test]
    fn prompts_do_not_change_plain_custom_terminators() {
        let mut e = Expander::new_configured(
            vec![
                prompted(";form", "{{value}}"),
                structured_match("two words", "plain"),
            ],
            TriggerMode::Space,
            vec!['\n'],
            None,
            256,
        );
        for c in "two words".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert_eq!(e.push_char('\n').unwrap().text, "plain");
        for c in ";form ".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert!(e.take_prompt().is_some());
    }

    #[test]
    fn physical_space_preserves_plain_custom_separator_behavior() {
        let mut plain = structured_match(";foo", "ordinary");
        plain.right_word = true;
        let mut e = Expander::new_configured(
            vec![plain, prompted(";form", "{{value}}")],
            TriggerMode::Immediate,
            vec![' '],
            Some(vec!['.']),
            256,
        );
        for c in ";foo ".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert!(e.take_prompt().is_none());
        e.reset();
        for c in ";foo".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert_eq!(e.push_char('.').unwrap().text, "ordinary.");
        e.reset();
        for c in ";form ".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert!(e.take_prompt().is_some());
    }

    #[test]
    fn prompt_never_expands_on_enter_tab_or_punctuation() {
        for c in ['\n', '\t', '.'] {
            let mut e = Expander::new_configured(
                vec![prompted(";form", "{{value}}")],
                TriggerMode::Space,
                vec![' ', '\n', '\t'],
                None,
                256,
            );
            for c in ";form".chars() {
                e.push_char(c);
            }
            assert!(e.push_char(c).is_none());
            assert!(e.take_prompt().is_none());
        }
    }

    #[test]
    fn prompted_manual_render_requires_handler_and_empty_answer_is_valid() {
        let mut e = Expander::new(
            vec![prompted(";form", "a{{value}}b")],
            TriggerMode::Immediate,
        );
        assert!(e
            .expansion_for_trigger(";form", None)
            .unwrap_err()
            .to_string()
            .starts_with("requires_prompt"));
        for c in ";form ".chars() {
            e.push_char(c);
        }
        let prompt = e.take_prompt().unwrap();
        assert!(prompt.render(&HashMap::new()).is_err());
        assert_eq!(
            prompt
                .render(&HashMap::from([("value".into(), String::new())]))
                .unwrap()
                .text,
            "ab "
        );
    }

    fn make_expander() -> Expander {
        Expander::new(
            vec![
                ("/mail".to_string(), "user@example.com".to_string()),
                ("/sig".to_string(), "Best regards,\nSilouan".to_string()),
            ],
            TriggerMode::Immediate,
        )
    }

    #[test]
    fn test_no_match_on_partial_trigger() {
        let mut e = make_expander();
        for c in "/mai".chars() {
            let result = e.push_char(c);
            assert_eq!(result, None, "partial trigger should not match");
        }
    }

    #[test]
    fn expands_selected_trigger_without_deleting_typed_text() {
        let e = Expander::new(
            vec![(";bold".to_string(), "**$|$**".to_string())],
            TriggerMode::Immediate,
        );
        assert_eq!(
            e.expansion_for_trigger(";bold", None).unwrap(),
            Some(Expansion {
                delete_count: 0,
                text: "****".to_string(),
                cursor_back: 2,
                undo_text: String::new(),
            })
        );
    }

    #[test]
    fn test_match_on_complete_trigger() {
        let mut e = make_expander();
        let mut result = None;
        for c in "/mail".chars() {
            result = e.push_char(c);
        }
        assert_eq!(
            result,
            Some(Expansion {
                delete_count: 5,
                text: "user@example.com".to_string(),
                cursor_back: 0,
                undo_text: "/mail".to_string(),
            })
        );
    }

    #[test]
    fn test_backspace_prevents_match() {
        let mut e = make_expander();
        // Type /mai
        for c in "/mai".chars() {
            e.push_char(c);
        }
        // Backspace (removes 'i')
        e.pop_char();
        // Type 'l'. Buffer is now "/mal", not "/mail".
        let result = e.push_char('l');
        assert_eq!(
            result, None,
            "buffer is /mal after backspace, should not match"
        );
    }

    #[test]
    fn test_reset_prevents_match() {
        let mut e = make_expander();
        // Type /mai
        for c in "/mai".chars() {
            e.push_char(c);
        }
        // Arrow key / escape
        e.reset();
        // Type 'l'. Buffer is "l", not "/mail".
        let result = e.push_char('l');
        assert_eq!(
            result, None,
            "buffer cleared by reset, single 'l' should not match"
        );
    }

    #[test]
    fn test_reset_after_match_allows_new_match() {
        let mut e = make_expander();
        // First match
        for c in "/mail".chars() {
            e.push_char(c);
        }
        // After a match the buffer is cleared internally, but call reset explicitly too
        e.reset();
        // Retype the trigger. The second match should fire.
        let mut result = None;
        for c in "/mail".chars() {
            result = e.push_char(c);
        }
        assert_eq!(
            result,
            Some(Expansion {
                delete_count: 5,
                text: "user@example.com".to_string(),
                cursor_back: 0,
                undo_text: "/mail".to_string(),
            }),
            "second match after reset should fire"
        );
    }

    #[test]
    fn test_multiline_expansion_text() {
        let mut e = make_expander();
        let mut result = None;
        for c in "/sig".chars() {
            result = e.push_char(c);
        }
        assert_eq!(
            result,
            Some(Expansion {
                delete_count: 4,
                text: "Best regards,\nSilouan".to_string(),
                cursor_back: 0,
                undo_text: "/sig".to_string(),
            })
        );
    }

    #[test]
    fn test_delete_count_equals_trigger_char_count() {
        let mut e = make_expander();
        let mut result = None;
        for c in "/sig".chars() {
            result = e.push_char(c);
        }
        let expansion = result.expect("/sig should match");
        // "/sig" is 4 chars: '/', 's', 'i', 'g'
        assert_eq!(expansion.delete_count, 4);
    }

    #[test]
    fn test_buffer_capped_at_max_trigger_length() {
        // max trigger len is 5 ("/mail"). Type many chars before the trigger.
        let mut e = make_expander();
        // Type a bunch of unrelated chars first
        for c in "hello world this is some text ".chars() {
            e.push_char(c);
        }
        // Now type the trigger. The capped buffer should still produce a suffix match.
        let mut result = None;
        for c in "/mail".chars() {
            result = e.push_char(c);
        }
        assert_eq!(
            result,
            Some(Expansion {
                delete_count: 5,
                text: "user@example.com".to_string(),
                cursor_back: 0,
                undo_text: "/mail".to_string(),
            }),
            "trigger should still match even after long prefix input"
        );
    }

    #[test]
    fn test_multiple_triggers() {
        let mut e = Expander::new(
            vec![
                ("/mail".to_string(), "user@example.com".to_string()),
                ("/phone".to_string(), "+1-555-0100".to_string()),
            ],
            TriggerMode::Immediate,
        );
        let mut result = None;
        for c in "/phone".chars() {
            result = e.push_char(c);
        }
        assert_eq!(
            result,
            Some(Expansion {
                delete_count: 6,
                text: "+1-555-0100".to_string(),
                cursor_back: 0,
                undo_text: "/phone".to_string(),
            })
        );
    }

    fn exp(pairs: &[(&str, &str)]) -> Expander {
        Expander::new(
            pairs
                .iter()
                .map(|(t, e)| (t.to_string(), e.to_string()))
                .collect(),
            TriggerMode::Immediate,
        )
    }

    #[test]
    fn test_update_replaces_expansions_and_clears_buffer() {
        let mut e = exp(&[("/mail", "a@b.com")]);
        // Partially type the old trigger
        e.push_char('/');
        e.push_char('m');
        // Update to a completely different set
        e.update(
            vec![("/phone".to_string(), "123456".to_string())],
            TriggerMode::Immediate,
        );
        // Old trigger should no longer fire
        e.push_char('a');
        e.push_char('i');
        assert!(e.push_char('l').is_none()); // "/mail" not in new config
                                             // New trigger should fire
        let mut e2 = Expander::new(
            vec![("/phone".to_string(), "123456".to_string())],
            TriggerMode::Immediate,
        );
        for c in "/phone".chars() {
            e2.push_char(c);
        }
        // last push:
        let mut e3 = Expander::new(
            vec![("/phone".to_string(), "123456".to_string())],
            TriggerMode::Immediate,
        );
        let chars: Vec<char> = "/phone".chars().collect();
        let last = chars.last().copied().unwrap();
        for &c in &chars[..chars.len() - 1] {
            e3.push_char(c);
        }
        assert!(e3.push_char(last).is_some());
    }

    #[test]
    fn test_empty_expansions_does_not_panic() {
        let mut e = Expander::new(Vec::<(String, String)>::new(), TriggerMode::Immediate);
        assert!(e.push_char('/').is_none());
        assert!(e.push_char('m').is_none());
        e.pop_char();
        e.reset();
        // update to non-empty and back
        e.update(
            vec![("/x".to_string(), "y".to_string())],
            TriggerMode::Immediate,
        );
        e.update(Vec::<(String, String)>::new(), TriggerMode::Immediate);
        assert!(e.push_char('x').is_none());
    }

    #[test]
    fn test_space_mode_waits_for_terminator_and_removes_it() {
        let mut e = Expander::new(
            vec![(";mail".to_string(), "user@example.com".to_string())],
            TriggerMode::Space,
        );
        for c in ";mail".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert_eq!(
            e.push_char(' '),
            Some(Expansion {
                delete_count: 6,
                text: "user@example.com".to_string(),
                cursor_back: 0,
                undo_text: ";mail ".to_string(),
            })
        );
    }

    #[test]
    fn terminated_mode_can_use_enter_instead_of_space() {
        let mut e = Expander::new_configured(
            vec![(";mail".to_string(), "user@example.com".to_string())],
            TriggerMode::Space,
            vec!['\n'],
            None,
            256,
        );
        for c in ";mail".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert!(e.push_char(' ').is_none());
        for c in ";mail".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert_eq!(e.push_char('\n').unwrap().delete_count, 6);
    }

    #[test]
    fn configured_word_separators_control_boundaries() {
        let mut item = structured_match("cat", "animal");
        item.left_word = true;
        let mut expander = Expander::new_configured(
            vec![item],
            TriggerMode::Immediate,
            vec![' '],
            Some(vec!['.']),
            256,
        );
        for character in "-cat".chars() {
            assert!(expander.push_char(character).is_none());
        }
        expander.reset();
        let mut expansion = None;
        for character in ".cat".chars() {
            expansion = expander.push_char(character);
        }
        assert_eq!(expansion.unwrap().text, "animal");
    }

    #[test]
    fn test_space_mode_clears_buffer_after_unmatched_word() {
        let mut e = Expander::new(
            vec![(";mail".to_string(), "user@example.com".to_string())],
            TriggerMode::Space,
        );
        for c in ";mai ".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert!(e.push_char('l').is_none());
        assert!(e.push_char(' ').is_none());
    }

    #[test]
    fn propagate_case_matches_insensitively_and_transforms_replacement() {
        let mut item = structured_match(";hello", "good morning");
        item.propagate_case = true;
        let mut expander = Expander::new(vec![item.clone()], TriggerMode::Immediate);
        let mut result = None;
        for character in ";HELLO".chars() {
            result = expander.push_char(character);
        }
        assert_eq!(result.unwrap().text, "GOOD MORNING");

        item.uppercase_style = UppercaseStyle::CapitalizeWords;
        let mut expander = Expander::new(vec![item], TriggerMode::Immediate);
        result = None;
        for character in ";Hello".chars() {
            result = expander.push_char(character);
        }
        assert_eq!(result.unwrap().text, "Good Morning");
    }

    #[test]
    fn ordinary_matches_remain_case_sensitive() {
        let mut expander = Expander::new(
            vec![structured_match(";hello", "good morning")],
            TriggerMode::Immediate,
        );
        for character in ";HELLO".chars() {
            assert!(expander.push_char(character).is_none());
        }
    }

    #[test]
    fn test_cursor_marker_is_removed_and_offset_is_counted_in_chars() {
        let mut e = Expander::new(
            vec![(";fn".to_string(), "fn demo() {\n    $|$\n}".to_string())],
            TriggerMode::Immediate,
        );
        let mut expansion = None;
        for c in ";fn".chars() {
            expansion = e.push_char(c);
        }
        assert_eq!(
            expansion,
            Some(Expansion {
                delete_count: 3,
                text: "fn demo() {\n    \n}".to_string(),
                cursor_back: 2,
                undo_text: ";fn".to_string(),
            })
        );
    }

    #[test]
    fn left_word_rejects_a_trigger_inside_another_word() {
        let mut item = structured_match("cat", "animal");
        item.left_word = true;
        let mut e = Expander::new(vec![item], TriggerMode::Immediate);
        for c in "bobcat".chars() {
            assert!(e.push_char(c).is_none());
        }
        e.reset();
        assert!(e.push_char(' ').is_none());
        assert!(e.push_char('c').is_none());
        assert!(e.push_char('a').is_none());
        assert!(e.push_char('t').is_some());
    }

    #[test]
    fn right_word_waits_for_and_preserves_separator() {
        let mut item = structured_match("cat", "animal");
        item.right_word = true;
        let mut e = Expander::new(vec![item], TriggerMode::Immediate);
        for c in "cat".chars() {
            assert!(e.push_char(c).is_none());
        }
        assert_eq!(
            e.push_char('.'),
            Some(Expansion {
                delete_count: 4,
                text: "animal.".to_string(),
                cursor_back: 0,
                undo_text: "cat.".to_string(),
            })
        );
    }

    #[test]
    fn date_variable_uses_espanso_placeholder_syntax() {
        let mut item = structured_match(";year", "Year: {{current_year}}");
        item.vars.push(crate::config::Variable {
            name: "current_year".to_string(),
            kind: VariableKind::Date,
            inject_vars: true,
            params: crate::config::VariableParams {
                format: Some("%Y".to_string()),
                ..Default::default()
            },
        });
        let mut e = Expander::new(vec![item], TriggerMode::Immediate);
        let mut expansion = None;
        for c in ";year".chars() {
            expansion = e.push_char(c);
        }
        assert_eq!(
            expansion.unwrap().text,
            format!("Year: {}", chrono::Local::now().format("%Y"))
        );
    }

    #[test]
    fn nested_match_variables_render_referenced_snippets() {
        let shared = structured_match(";name", "Silouan");
        let mut greeting = structured_match(";greet", "Hello, {{person}}!");
        greeting.vars.push(crate::config::Variable {
            name: "person".into(),
            kind: VariableKind::Match,
            inject_vars: true,
            params: crate::config::VariableParams {
                trigger: Some(";name".into()),
                ..Default::default()
            },
        });
        let mut expander = Expander::new(vec![shared, greeting], TriggerMode::Immediate);
        let mut expansion = None;
        for character in ";greet".chars() {
            expansion = expander.push_char(character);
        }
        assert_eq!(expansion.unwrap().text, "Hello, Silouan!");
    }

    #[test]
    fn regex_triggers_render_named_captures() {
        let mut item = structured_match("unused", "Issue #{{number}}");
        item.triggers.clear();
        item.regex = Some(r"issue-(?P<number>\d{3})".into());
        let mut expander =
            Expander::new_configured(vec![item], TriggerMode::Immediate, vec![' '], None, 64);
        let mut expansion = None;
        for character in "please issue-123".chars() {
            expansion = expander.push_char(character);
        }
        let expansion = expansion.unwrap();
        assert_eq!(expansion.delete_count, 9);
        assert_eq!(expansion.text, "Issue #123");
        assert_eq!(expansion.undo_text, "issue-123");
    }

    #[test]
    fn duplicate_triggers_require_source_selection() {
        let mut first = structured_match(";same", "first");
        first.source = PathBuf::from("first.yml");
        let mut second = structured_match(";same", "second");
        second.source = PathBuf::from("second.yml");
        let mut expander = Expander::new(vec![first, second], TriggerMode::Immediate);
        let mut automatic = None;
        for character in ";same".chars() {
            automatic = expander.push_char(character);
        }
        assert!(automatic.is_none());
        assert!(expander.trigger_is_ambiguous(";same"));
        assert_eq!(
            expander
                .expansion_for_trigger(";same", Some("second.yml"))
                .unwrap()
                .unwrap()
                .text,
            "second"
        );
    }

    #[test]
    fn test_exact_match_not_confused_with_longer_trigger() {
        // With "/sig" and "/signal" configured, "/sig" should fire when typed,
        // NOT fire on typing "/signal" mid-stream.
        // (In practice Config prevents prefix conflicts, but Expander itself
        // handles the suffix match correctly. The longer trigger wins.)
        let mut e = exp(&[("/signal", "alarm"), ("/sig", "Best regards")]);
        // Type "/signal" fully
        let chars: Vec<char> = "/signal".chars().collect();
        let mut any_fired = false;
        for c in chars {
            if e.push_char(c).is_some() {
                any_fired = true;
            }
        }
        // "/signal" is longer, but "/sig" appears first in expansions.
        // The actual result depends on iteration order (first-match-wins).
        // We just assert one of them fires and no panic occurs.
        assert!(any_fired);
    }

    #[test]
    fn render_failure_does_not_emit_an_expansion_and_resets_input() {
        for word in [false, true] {
            let mut item = structured_match(";bad", "{{date}}");
            item.word = word;
            item.vars.push(crate::config::Variable {
                name: "date".into(),
                kind: VariableKind::Date,
                inject_vars: true,
                params: crate::config::VariableParams {
                    offset: Some(i64::MAX),
                    ..Default::default()
                },
            });
            let mut expander = Expander::new(vec![item], TriggerMode::Immediate);
            for c in ";bad ".chars() {
                assert!(expander.push_char(c).is_none());
            }
            assert!(expander.expansion_for_trigger(";bad", None).is_err());
            // The literal trigger itself reaches the failure path without word boundaries.
            if word {
                assert!(expander.buffer.is_empty());
            }
        }
    }

    #[test]
    fn echo_can_use_optional_regex_captures_and_literal_braces() {
        let mut item = structured_match("", "{{output}} {{id}}");
        item.triggers.clear();
        item.regex = Some("issue-(?P<id>[0-9]+)?".into());
        item.vars.push(crate::config::Variable {
            name: "output".into(),
            kind: VariableKind::Echo,
            inject_vars: true,
            params: crate::config::VariableParams {
                echo: Some(r"\{\{id}}={{id}}".into()),
                ..Default::default()
            },
        });
        let mut engine = Expander::new(vec![item], TriggerMode::Space);
        for (typed, expected) in [("issue-12 ", "{{id}}=12 12"), ("issue- ", "{{id}}= ")] {
            let output = typed
                .chars()
                .filter_map(|c| engine.push_char(c))
                .last()
                .unwrap();
            assert_eq!(output.text, expected);
        }
    }

    #[test]
    fn nested_date_variables_share_one_render_instant() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-12-31T23:59:59.999999999Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let date = crate::config::Variable {
            name: "stamp".into(),
            kind: VariableKind::Date,
            inject_vars: true,
            params: crate::config::VariableParams {
                format: Some("%Y-%m-%dT%H:%M:%S%.9fZ".into()),
                tz: Some("UTC".into()),
                ..Default::default()
            },
        };
        let mut child = structured_match(";child", "{{stamp}}");
        child.vars.push(date.clone());
        let mut parent = structured_match(";parent", "{{stamp}} {{nested}}");
        parent.vars = vec![
            date,
            crate::config::Variable {
                name: "nested".into(),
                kind: VariableKind::Match,
                inject_vars: true,
                params: crate::config::VariableParams {
                    trigger: Some(";child".into()),
                    ..Default::default()
                },
            },
        ];
        let compiled = compile_matches(vec![parent, child]);
        let item = compiled
            .iter()
            .find(|item| item.trigger == ";parent")
            .unwrap();
        assert_eq!(
            render(&compiled, item, &HashMap::new(), now).unwrap(),
            "2026-12-31T23:59:59.999999999Z 2026-12-31T23:59:59.999999999Z"
        );
    }
}
