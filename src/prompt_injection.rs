//! Prepared prompted replacement. The worker owns text; public handles contain no values.
use crate::injector::{ComposeTiming, KeyboardTransport, KeymapLookup};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PreparedPrompt(pub(crate) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptError {
    Busy,
    Unavailable,
    Expired,
    Cancelled,
    InvalidOutput,
    UnsupportedText,
    PreparationFailed,
    InvalidHandle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptOutcome {
    /// All keyboard operations were dispatched; applications do not acknowledge them.
    Issued,
    FailedBeforeMutation(PromptError),
    /// At least one editing operation may have reached the application.
    Indeterminate,
}

/// Cancellation and first mutation contend for a single atomic authorization.
#[derive(Debug, Default)]
pub(crate) struct CommitToken(AtomicU8);

impl CommitToken {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn is_committing(&self) -> bool {
        self.0.load(Ordering::Acquire) == 2
    }

    fn claim(&self, deadline: Instant) -> Result<(), PromptError> {
        if Instant::now() >= deadline {
            self.cancel();
            return Err(PromptError::Expired);
        }
        self.0
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| PromptError::Cancelled)
    }
}

pub(crate) trait PromptTransport {
    fn key(&mut self, code: u16, value: i32) -> Result<(), ()>;
    fn text(&mut self, text: &str) -> Result<(), ()>;
    fn flush(&mut self) -> Result<(), ()>;
}

/// Uinput strokes are resolved before mutation. Wayland uses a prepared text map.
pub(crate) struct PreparedOperation {
    pub(crate) handle: PreparedPrompt,
    pub(crate) deadline: Instant,
    pub(crate) delete_count: usize,
    pub(crate) text: String,
    pub(crate) text_keys: Option<Vec<(u16, i32)>>,
    pub(crate) cursor_back: usize,
    pub(crate) delay_ms: u64,
    pub(crate) settle_ms: u64,
}

impl PreparedOperation {
    pub(crate) fn execute(
        self,
        transport: &mut impl PromptTransport,
        token: &CommitToken,
        deadline: Instant,
    ) -> PromptOutcome {
        std::thread::sleep(std::time::Duration::from_millis(self.settle_ms));
        if let Err(error) = token.claim(deadline.min(self.deadline)) {
            return PromptOutcome::FailedBeforeMutation(error);
        }
        // Claim immediately precedes first editing operation. Any native failure
        // is uncertain, including failure of the first press or final flush.
        let result = (|| {
            for _ in 0..self.delete_count {
                transport.key(14, 1)?;
                transport.key(14, 0)?;
            }
            transport.flush()?;
            if let Some(keys) = &self.text_keys {
                for &(code, value) in keys {
                    transport.key(code, value)?;
                }
            } else {
                transport.text(&self.text)?;
            }
            for _ in 0..self.cursor_back {
                transport.key(105, 1)?;
                transport.key(105, 0)?;
            }
            transport.flush()
        })();
        if result.is_err() {
            // Best effort releases even when the backend fails mid-press.
            for code in [14, 105, 42, 54, 100] {
                let _ = transport.key(code, 0);
            }
            if let Some(keys) = &self.text_keys {
                let codes = keys
                    .iter()
                    .map(|(code, _)| *code)
                    .collect::<std::collections::BTreeSet<_>>();
                for code in codes {
                    let _ = transport.key(code, 0);
                }
            }
            let _ = transport.flush();
            PromptOutcome::Indeterminate
        } else {
            PromptOutcome::Issued
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[derive(Default)]
    struct Fake {
        calls: usize,
        fail_at: Option<usize>,
        mutation: bool,
    }
    impl Fake {
        fn call(&mut self) -> Result<(), ()> {
            self.calls += 1;
            if self.fail_at == Some(self.calls) {
                Err(())
            } else {
                Ok(())
            }
        }
    }
    impl PromptTransport for Fake {
        fn key(&mut self, _: u16, value: i32) -> Result<(), ()> {
            self.mutation |= value == 1;
            self.call()
        }
        fn text(&mut self, _: &str) -> Result<(), ()> {
            self.call()
        }
        fn flush(&mut self) -> Result<(), ()> {
            self.call()
        }
    }
    fn operation() -> PreparedOperation {
        PreparedOperation {
            handle: PreparedPrompt(1),
            deadline: Instant::now() + Duration::from_secs(2),
            delete_count: 1,
            text: "value ".into(),
            text_keys: None,
            cursor_back: 1,
            delay_ms: 0,
            settle_ms: 0,
        }
    }
    #[test]
    fn cancelled_and_expired_never_touch_transport() {
        for expired in [false, true] {
            let token = CommitToken::new();
            if !expired {
                assert!(token.cancel());
            }
            let deadline = if expired {
                Instant::now()
            } else {
                operation().deadline
            };
            let mut fake = Fake::default();
            assert!(matches!(
                operation().execute(&mut fake, &token, deadline),
                PromptOutcome::FailedBeforeMutation(_)
            ));
            assert_eq!(fake.calls, 0);
        }
    }
    #[test]
    fn every_native_failure_is_indeterminate() {
        for fail_at in 1..=7 {
            let mut fake = Fake {
                fail_at: Some(fail_at),
                ..Fake::default()
            };
            assert_eq!(
                operation().execute(&mut fake, &CommitToken::new(), operation().deadline),
                PromptOutcome::Indeterminate
            );
        }
    }
    #[test]
    fn cancellation_races_first_mutation_with_one_winner() {
        for _ in 0..100 {
            let token = std::sync::Arc::new(CommitToken::new());
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let cancelling = std::sync::Arc::clone(&token);
            let gate = std::sync::Arc::clone(&barrier);
            let thread = std::thread::spawn(move || {
                gate.wait();
                cancelling.cancel()
            });
            barrier.wait();
            let mut fake = Fake::default();
            let result = operation().execute(&mut fake, &token, operation().deadline);
            let cancelled = thread.join().unwrap();
            if cancelled {
                assert_eq!(
                    result,
                    PromptOutcome::FailedBeforeMutation(PromptError::Cancelled)
                );
                assert_eq!(fake.calls, 0);
            } else {
                assert_eq!(result, PromptOutcome::Issued);
                assert!(token.is_committing());
            }
        }
    }

    #[test]
    fn successful_claim_cannot_be_cancelled_or_reused() {
        let token = CommitToken::new();
        assert_eq!(
            operation().execute(&mut Fake::default(), &token, operation().deadline),
            PromptOutcome::Issued
        );
        assert!(token.is_committing());
        assert!(!token.cancel());
        let mut fake = Fake::default();
        assert!(matches!(
            operation().execute(&mut fake, &token, operation().deadline),
            PromptOutcome::FailedBeforeMutation(PromptError::Cancelled)
        ));
        assert_eq!(fake.calls, 0);
    }
}

pub(crate) struct PromptKeyboard<'a> {
    pub(crate) keyboard: &'a mut dyn KeyboardTransport,
    pub(crate) delay_ms: u64,
}

impl PromptTransport for PromptKeyboard<'_> {
    fn key(&mut self, code: u16, value: i32) -> std::result::Result<(), ()> {
        self.keyboard.send_key(code, value).map_err(|_| ())?;
        if value == 0 {
            let delay = self.delay_ms.saturating_add(u64::from(
                matches!(code, 42 | 54 | 100) && self.delay_ms > 0,
            ));
            std::thread::sleep(Duration::from_millis(delay));
        }
        Ok(())
    }
    fn text(&mut self, text: &str) -> std::result::Result<(), ()> {
        self.keyboard
            .send_text(
                text,
                self.delay_ms,
                false,
                ComposeTiming {
                    delay_ms: 0,
                    settle_ms: 0,
                },
            )
            .map_err(|_| ())
    }
    fn flush(&mut self) -> std::result::Result<(), ()> {
        self.keyboard.flush().map_err(|_| ())
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_prompt_operation(
    keyboard: &mut dyn KeyboardTransport,
    backend: &str,
    lookup: &KeymapLookup,
    configured_chars: &str,
    original: String,
    text: String,
    cursor_back: usize,
    settle_ms: u64,
    deadline: Instant,
    handle: PreparedPrompt,
    delay_ms: u64,
) -> std::result::Result<PreparedOperation, PromptError> {
    if Instant::now() >= deadline {
        return Err(PromptError::Expired);
    }
    if original.is_empty()
        || original.len() > 4000
        || text.len() > 4000
        || !text.ends_with(' ')
        || cursor_back > text.chars().count()
        || text
            .chars()
            .any(|value| value.is_control() && !matches!(value, '\n' | '\t'))
    {
        return Err(PromptError::InvalidOutput);
    }
    // Some applications truncate virtual-keyboard non-BMP keysyms to UTF-16.
    // Without an application-independent lossless path, reject before deletion.
    if text.chars().any(|ch| u32::from(ch) > 0xffff) {
        return Err(PromptError::UnsupportedText);
    }
    let text_keys = if backend == "wayland" {
        let characters = format!("{configured_chars}{text}");
        keyboard
            .refresh_text_keymap(&characters)
            .map_err(|_| PromptError::PreparationFailed)?;
        keyboard
            .validate_prompt_text(&text)
            .map_err(|_| PromptError::UnsupportedText)?;
        None
    } else {
        let mut keys = Vec::new();
        for character in text.chars() {
            let (code, level) = match character {
                '\n' => (28, 0),
                '\t' => (15, 0),
                _ => {
                    let info = lookup
                        .lookup(character)
                        .ok_or(PromptError::UnsupportedText)?;
                    (
                        u16::try_from(info.evdev_code).map_err(|_| PromptError::UnsupportedText)?,
                        info.level,
                    )
                }
            };
            if level > 3 || !(1..=248).contains(&code) {
                return Err(PromptError::UnsupportedText);
            }
            let shift = matches!(level, 1 | 3);
            let altgr = matches!(level, 2 | 3);
            if altgr {
                keys.push((100, 1));
            }
            if shift {
                keys.push((42, 1));
            }
            keys.extend([(code, 1), (code, 0)]);
            if shift {
                keys.push((42, 0));
            }
            if altgr {
                keys.push((100, 0));
            }
        }
        Some(keys)
    };
    // Keyboard cursor movement is only predictable for ordinary single-line ASCII.
    let cursor_back = if text.is_ascii() && !text.contains(['\n', '\t']) {
        cursor_back
    } else {
        0
    };
    let count = original.chars().count() * 2
        + text_keys
            .as_ref()
            .map_or(text.chars().count() * 2, Vec::len)
        + cursor_back * 2;
    if count > 40000 {
        return Err(PromptError::InvalidOutput);
    }
    if Instant::now() >= deadline {
        return Err(PromptError::Expired);
    }
    Ok(PreparedOperation {
        handle,
        deadline,
        delete_count: original.chars().count(),
        text,
        text_keys,
        cursor_back,
        delay_ms,
        settle_ms,
    })
}

#[cfg(test)]
mod preparation_tests {
    use super::*;

    #[derive(Default)]
    struct Preflight {
        configured: String,
        failed: bool,
        mutation: bool,
    }
    impl KeyboardTransport for Preflight {
        fn send_key(&mut self, _: u16, _: i32) -> anyhow::Result<()> {
            self.mutation = true;
            Ok(())
        }
        fn refresh_text_keymap(&mut self, characters: &str) -> anyhow::Result<()> {
            self.configured = characters.into();
            if self.failed {
                anyhow::bail!("private backend detail");
            }
            Ok(())
        }
        fn validate_prompt_text(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }
    fn lookup() -> KeymapLookup {
        KeymapLookup::build(
            r#"xkb_keymap {
            xkb_keycodes "test" { minimum = 8; maximum = 65; <A> = 38; <SP> = 65; };
            xkb_types "test" { include "complete" };
            xkb_compatibility "test" { include "complete" };
            xkb_symbols "test" { key <A> { [a] }; key <SP> { [space] }; };
        };"#,
        )
    }
    fn prepare(
        fake: &mut Preflight,
        backend: &str,
        text: &str,
    ) -> std::result::Result<PreparedOperation, PromptError> {
        prepare_prompt_operation(
            fake,
            backend,
            &lookup(),
            "configuredΩ",
            "trigger ".into(),
            text.into(),
            1,
            0,
            Instant::now() + Duration::from_secs(2),
            PreparedPrompt(1),
            0,
        )
    }
    #[test]
    fn wayland_prepares_unicode_without_mutation_and_preserves_configured_chars() {
        let mut fake = Preflight::default();
        let prepared = prepare(&mut fake, "wayland", "éΩ\nvalue ").unwrap();
        assert!(fake.configured.starts_with("configuredΩ"));
        assert!(fake.configured.contains('é'));
        assert!(!fake.mutation);
        assert_eq!(prepared.cursor_back, 0);
    }
    #[test]
    fn invalid_or_unsupported_outputs_never_delete() {
        let mut fake = Preflight::default();
        assert!(matches!(
            prepare(&mut fake, "uinput", "😀 "),
            Err(PromptError::UnsupportedText)
        ));
        assert!(matches!(
            prepare(&mut fake, "wayland", "without trailing space"),
            Err(PromptError::InvalidOutput)
        ));
        assert!(matches!(
            prepare(&mut fake, "wayland", &format!("{} ", "a".repeat(4000))),
            Err(PromptError::InvalidOutput)
        ));
        assert!(matches!(
            prepare(&mut fake, "wayland", "\u{1b} "),
            Err(PromptError::InvalidOutput)
        ));
        assert!(matches!(
            prepare(&mut fake, "wayland", "🙂 "),
            Err(PromptError::UnsupportedText)
        ));
        fake.failed = true;
        assert!(matches!(
            prepare(&mut fake, "wayland", "a "),
            Err(PromptError::PreparationFailed)
        ));
        assert!(!fake.mutation);
    }
    #[test]
    fn uinput_resolves_every_key_before_mutation() {
        let mut fake = Preflight::default();
        let prepared = prepare(&mut fake, "uinput", "a ").unwrap();
        assert_eq!(
            prepared.text_keys,
            Some(vec![(30, 1), (30, 0), (57, 1), (57, 0)])
        );
        assert!(!fake.mutation);
    }
}
