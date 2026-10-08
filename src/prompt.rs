//! Frontend-independent deferred prompt lifecycle. Only the daemon owns this state.
use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::expander::DeferredMatch;
use crate::fields::{FieldKind, PromptSettings};
use crate::prompt_injection::{CommitToken, PreparedPrompt, PromptOutcome};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Register {
        version: u32,
    },
    Status {
        version: u32,
    },
    Renew {
        version: u32,
        session: String,
    },
    Ack {
        version: u32,
        session: String,
        id: String,
    },
    Submit {
        version: u32,
        session: String,
        id: String,
        answers: HashMap<String, Answer>,
    },
    Closed {
        version: u32,
        session: String,
        id: String,
    },
    Cancel {
        version: u32,
        session: String,
        id: String,
    },
}

impl Message {
    fn version(&self) -> u32 {
        match self {
            Self::Register { version }
            | Self::Status { version }
            | Self::Renew { version, .. }
            | Self::Ack { version, .. }
            | Self::Submit { version, .. }
            | Self::Closed { version, .. }
            | Self::Cancel { version, .. } => *version,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Answer {
    Text { value: String },
    Choice { option: String },
}

pub enum Effect {
    Reply {
        connection: u64,
        message: Value,
    },
    Close(u64),
    Prepare {
        id: String,
        original: String,
        text: String,
        cursor_back: usize,
        deadline: Instant,
    },
    Discard(PreparedPrompt),
    Commit {
        id: String,
        prepared: PreparedPrompt,
        token: Arc<CommitToken>,
        deadline: Instant,
    },
}

struct Handler {
    connection: u64,
    session: String,
    expires: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    TriggerRelease,
    Ack,
    Editing,
    Preparing,
    SubmitRelease,
    Closed,
    Settling,
    Queued,
}

struct Transaction {
    id: String,
    connection: u64,
    session: String,
    candidate: DeferredMatch,
    phase: Phase,
    step_deadline: Instant,
    completion: Option<Instant>,
    prepared: Option<PreparedPrompt>,
    token: Arc<CommitToken>,
    terminal_sent: bool,
}

pub struct Controller {
    settings: PromptSettings,
    nonce: String,
    counter: u64,
    handler: Option<Handler>,
    pending: Option<Transaction>,
}

fn after(now: Instant, millis: u64) -> Instant {
    now + Duration::from_millis(millis)
}
fn reply(connection: u64, message: Value) -> Effect {
    Effect::Reply {
        connection,
        message,
    }
}
fn error(connection: u64, code: &'static str) -> Effect {
    reply(connection, json!({"version":1,"type":"error","code":code}))
}

impl Controller {
    pub fn new(settings: PromptSettings) -> anyhow::Result<Self> {
        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let nonce = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            settings,
            nonce,
            counter: 0,
            handler: None,
            pending: None,
        })
    }

    fn identifier(&mut self) -> Option<String> {
        self.counter = self.counter.checked_add(1)?;
        Some(format!("{}-{:016x}", self.nonce, self.counter))
    }

    pub fn commit_authorized(&self, id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.id == id && p.phase == Phase::Queued && !p.terminal_sent)
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn settings(&self) -> &PromptSettings {
        &self.settings
    }

    pub fn configure(&mut self, settings: PromptSettings) {
        self.settings = settings;
    }

    pub fn admit(&mut self, candidate: DeferredMatch, now: Instant) -> Vec<Effect> {
        let mut effects = self.expire(now);
        if self.pending.is_some() || self.handler.is_none() {
            return effects;
        }
        let Some(id) = self.identifier() else {
            effects.extend(self.cancel("identifier_exhausted"));
            return effects;
        };
        tracing::info!(request_id = %id, "Prompt admitted");
        self.pending = Some(Transaction {
            id,
            connection: self.handler.as_ref().unwrap().connection,
            session: self.handler.as_ref().unwrap().session.clone(),
            candidate,
            phase: Phase::TriggerRelease,
            step_deadline: after(now, self.settings.release_timeout_ms),
            completion: None,
            prepared: None,
            token: Arc::new(CommitToken::new()),
            terminal_sent: false,
        });
        effects
    }

    pub fn handle(
        &mut self,
        connection: u64,
        wire: Value,
        now: Instant,
        keys_up: bool,
    ) -> Vec<Effect> {
        let mut effects = self.expire(now);
        if serde_json::to_vec(&wire).map_or(true, |v| v.len() > self.settings.max_frame_bytes) {
            effects.push(error(connection, "frame_too_large"));
            effects.push(Effect::Close(connection));
            return effects;
        }
        let message: Message = match serde_json::from_value(wire) {
            Ok(value) => value,
            Err(_) => {
                effects.push(error(connection, "invalid_message"));
                if self
                    .handler
                    .as_ref()
                    .is_none_or(|h| h.connection != connection)
                {
                    effects.push(Effect::Close(connection));
                }
                return effects;
            }
        };
        if message.version() != 1 {
            effects.push(error(connection, "unsupported_version"));
            if self
                .handler
                .as_ref()
                .is_none_or(|h| h.connection != connection)
            {
                effects.push(Effect::Close(connection));
            }
            return effects;
        }
        match message {
            Message::Status { .. } => {
                effects.push(reply(connection, json!({"version":1,"type":"status",
                    "available":self.handler.is_some(),"pending":self.pending.as_ref().map(|p| &p.id)})));
            }
            Message::Register { .. } => {
                if self.handler.is_some() {
                    effects.push(error(connection, "busy"));
                } else if let Some(session) = self.identifier() {
                    effects.push(reply(
                        connection,
                        json!({"version":1,"type":"registered",
                        "session":session,"lease_renewal_ms":self.settings.lease_renewal_ms,
                        "lease_expiry_ms":self.settings.lease_expiry_ms}),
                    ));
                    self.handler = Some(Handler {
                        connection,
                        session,
                        expires: after(now, self.settings.lease_expiry_ms),
                    });
                } else {
                    effects.push(error(connection, "identifier_exhausted"));
                }
            }
            Message::Renew { session, .. } => {
                if let Some(handler) = self
                    .handler
                    .as_mut()
                    .filter(|h| h.connection == connection && h.session == session)
                {
                    handler.expires = after(now, self.settings.lease_expiry_ms);
                } else {
                    effects.push(error(connection, "wrong_session"));
                }
            }
            Message::Ack { session, id, .. } => {
                if !self.owns(connection, &session, &id) {
                    effects.push(error(connection, "stale_request"));
                } else if let Some(p) = self.pending.as_mut().filter(|p| p.phase == Phase::Ack) {
                    p.phase = Phase::Editing;
                    p.completion = Some(after(now, self.settings.completion_timeout_ms));
                } else {
                    effects.push(error(connection, "invalid_state"));
                }
            }
            Message::Submit {
                session,
                id,
                answers,
                ..
            } => {
                if !self.owns(connection, &session, &id) {
                    effects.push(error(connection, "stale_request"));
                } else if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.phase == Phase::Editing)
                {
                    let p = self.pending.as_ref().unwrap();
                    match self.resolve(p, answers).and_then(|values| p.candidate.render(&values).map_err(|_| "render_failed")) {
                        Ok(expansion) if expansion.text.len() <= self.settings.max_output_bytes => {
                            let p = self.pending.as_mut().unwrap();
                            p.phase = Phase::Preparing;
                            effects.push(Effect::Prepare { id, original: expansion.undo_text, text: expansion.text,
                                cursor_back: expansion.cursor_back, deadline: p.completion.unwrap() });
                        }
                        Ok(_) => effects.push(reply(connection,json!({"version":1,"type":"validation_error","id":id,"code":"output_too_large"}))),
                        Err(code) => effects.push(reply(connection,json!({"version":1,"type":"validation_error","id":id,"code":code}))),
                    }
                } else {
                    effects.push(error(connection, "invalid_state"));
                }
            }
            Message::Closed { session, id, .. } => {
                if !self.owns(connection, &session, &id) {
                    effects.push(error(connection, "stale_request"));
                } else if let Some(p) = self
                    .pending
                    .as_mut()
                    .filter(|p| p.phase == Phase::Closed && keys_up)
                {
                    p.phase = Phase::Settling;
                    p.step_deadline = after(now, self.settings.focus_settle_ms);
                } else {
                    effects.push(error(connection, "invalid_state"));
                }
            }
            Message::Cancel { session, id, .. } => {
                if self.owns(connection, &session, &id) {
                    effects.extend(self.cancel("user_cancelled"));
                } else {
                    effects.push(error(connection, "stale_request"));
                }
            }
        }
        effects.extend(self.advance(now, keys_up));
        if self
            .handler
            .as_ref()
            .is_none_or(|h| h.connection != connection)
        {
            effects.push(Effect::Close(connection));
        }
        effects
    }

    fn owns(&self, connection: u64, session: &str, id: &str) -> bool {
        self.handler
            .as_ref()
            .is_some_and(|h| h.connection == connection && h.session == session)
            && self.pending.as_ref().is_some_and(|p| {
                p.id == id && p.connection == connection && p.session == session && !p.terminal_sent
            })
    }

    fn resolve(
        &self,
        p: &Transaction,
        answers: HashMap<String, Answer>,
    ) -> Result<HashMap<String, String>, &'static str> {
        if answers.len() != p.candidate.fields.len() {
            return Err("answer_presence");
        }
        let mut values = HashMap::new();
        for field in &p.candidate.fields {
            let value = match (field.kind, answers.get(&field.id)) {
                (FieldKind::Text, Some(Answer::Text { value }))
                    if value.len() <= self.settings.max_answer_bytes =>
                {
                    value.clone()
                }
                (FieldKind::Choice, Some(Answer::Choice { option }))
                    if option.len() <= self.settings.max_answer_bytes =>
                {
                    field
                        .options
                        .iter()
                        .find(|o| &o.id == option)
                        .ok_or("invalid_option")?
                        .value
                        .clone()
                }
                (_, None) => return Err("answer_presence"),
                _ => return Err("invalid_answer"),
            };
            values.insert(field.id.clone(), value);
        }
        Ok(values)
    }

    pub fn prepared(
        &mut self,
        id: &str,
        result: Result<PreparedPrompt, &'static str>,
        now: Instant,
        keys_up: bool,
    ) -> Vec<Effect> {
        let mut effects = self.expire(now);
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| p.id == id && p.phase == Phase::Preparing)
        {
            if let Ok(prepared) = result {
                effects.push(Effect::Discard(prepared));
            }
            return effects;
        }
        let p = self.pending.as_mut().unwrap();
        match result {
            Ok(prepared) => {
                p.prepared = Some(prepared);
                p.phase = Phase::SubmitRelease;
                p.step_deadline = after(now, self.settings.release_timeout_ms);
            }
            Err(code) => {
                p.phase = Phase::Editing;
                if let Some(h) = &self.handler {
                    effects.push(reply(
                        h.connection,
                        json!({"version":1,"type":"validation_error","id":id,"code":code}),
                    ));
                }
            }
        }
        effects.extend(self.advance(now, keys_up));
        effects
    }

    pub fn physical_key(&mut self, pressed: bool, now: Instant, keys_up: bool) -> Vec<Effect> {
        let mut effects = self.expire(now);
        if pressed
            && self.pending.as_ref().is_some_and(|p| {
                matches!(
                    p.phase,
                    Phase::TriggerRelease | Phase::Closed | Phase::Settling | Phase::Queued
                )
            })
        {
            effects.extend(self.cancel("input_changed"));
        }
        effects.extend(self.advance(now, keys_up));
        effects
    }

    pub fn tick(&mut self, now: Instant, keys_up: bool) -> Vec<Effect> {
        let mut effects = self.expire(now);
        effects.extend(self.advance(now, keys_up));
        effects
    }

    fn advance(&mut self, now: Instant, keys_up: bool) -> Vec<Effect> {
        let Some(h) = &self.handler else {
            return Vec::new();
        };
        let Some(p) = self.pending.as_mut() else {
            return Vec::new();
        };
        if p.terminal_sent {
            return Vec::new();
        }
        match p.phase {
            Phase::TriggerRelease if keys_up => {
                let request = json!({"version":1,"type":"request","session":h.session,"id":p.id,"fields":p.candidate.fields});
                if serde_json::to_vec(&request).map_or(true, |v| {
                    v.len() + 1
                        > self
                            .settings
                            .max_request_bytes
                            .min(self.settings.max_frame_bytes)
                }) {
                    return self.cancel("request_too_large");
                }
                p.phase = Phase::Ack;
                p.step_deadline = after(now, self.settings.ack_timeout_ms);
                vec![reply(h.connection, request)]
            }
            Phase::SubmitRelease if keys_up => {
                p.phase = Phase::Closed;
                p.step_deadline = after(now, self.settings.close_timeout_ms);
                vec![reply(
                    h.connection,
                    json!({"version":1,"type":"accepted","id":p.id}),
                )]
            }
            Phase::Settling if keys_up && now >= p.step_deadline => {
                p.phase = Phase::Queued;
                vec![Effect::Commit {
                    id: p.id.clone(),
                    prepared: p.prepared.take().unwrap(),
                    token: Arc::clone(&p.token),
                    deadline: p.completion.unwrap().min(h.expires),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn expire(&mut self, now: Instant) -> Vec<Effect> {
        if self.handler.as_ref().is_some_and(|h| now >= h.expires) {
            let connection = self.handler.as_ref().unwrap().connection;
            let mut effects = self.cancel("lease_expired");
            self.handler = None;
            effects.push(Effect::Close(connection));
            return effects;
        }
        let reason = self.pending.as_ref().and_then(|p| {
            if p.terminal_sent {
                return None;
            }
            if p.completion.is_some_and(|d| now >= d) {
                return Some("completion_timeout");
            }
            if now >= p.step_deadline {
                match p.phase {
                    Phase::TriggerRelease | Phase::SubmitRelease => Some("release_timeout"),
                    Phase::Ack => Some("ack_timeout"),
                    Phase::Closed => Some("close_timeout"),
                    _ => None,
                }
            } else {
                None
            }
        });
        reason.map_or_else(Vec::new, |code| self.cancel(code))
    }

    pub fn disconnect(&mut self, connection: u64) -> Vec<Effect> {
        if self
            .handler
            .as_ref()
            .is_none_or(|h| h.connection != connection)
        {
            return Vec::new();
        }
        let effects = self.cancel("handler_disconnected");
        self.handler = None;
        effects
    }

    pub fn cancel(&mut self, code: &'static str) -> Vec<Effect> {
        let Some(mut p) = self.pending.take() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        p.token.cancel();
        let cancelled = !p.token.is_committing();
        if let Some(prepared) = p.prepared.take() {
            effects.push(Effect::Discard(prepared));
        }
        if !p.terminal_sent {
            let outcome = if cancelled {
                "cancelled"
            } else {
                "indeterminate"
            };
            effects.push(reply(
                p.connection,
                json!({"version":1,"type":"result","id":p.id,"outcome":outcome,"code":code}),
            ));
            tracing::info!(request_id = %p.id, outcome, code, "Prompt finished");
            p.terminal_sent = true;
        }
        // Queued or claimed operations retain exclusive ownership until their result arrives.
        if p.phase == Phase::Queued {
            self.pending = Some(p);
        }
        effects
    }

    pub fn finished(&mut self, id: &str, outcome: PromptOutcome) -> Vec<Effect> {
        if !self
            .pending
            .as_ref()
            .is_some_and(|p| p.id == id && p.phase == Phase::Queued)
        {
            return Vec::new();
        }
        let p = self.pending.take().unwrap();
        if p.terminal_sent {
            return Vec::new();
        }
        let (outcome, code) = match outcome {
            PromptOutcome::Issued => ("issued", None),
            PromptOutcome::FailedBeforeMutation(_) => ("failed", Some("injection_unavailable")),
            PromptOutcome::Indeterminate => ("indeterminate", Some("insertion_uncertain")),
        };
        tracing::info!(request_id = %p.id, outcome, "Prompt finished");
        vec![reply(
            p.connection,
            json!({"version":1,"type":"result","id":id,"outcome":outcome,"code":code}),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(effects: Vec<Effect>) -> Vec<Value> {
        effects
            .into_iter()
            .filter_map(|e| match e {
                Effect::Reply { message, .. } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn candidate() -> DeferredMatch {
        let item = crate::config::Match {
            triggers: vec![";form".into()],
            regex: None,
            label: None,
            search_terms: vec![],
            replace: "Hello {{name}}".into(),
            vars: vec![],
            fields: vec![
                serde_json::from_value(json!({"id":"name","label":"Name","type":"text"})).unwrap(),
            ],
            word: false,
            left_word: false,
            right_word: false,
            propagate_case: false,
            uppercase_style: crate::config::UppercaseStyle::Uppercase,
            source: std::path::PathBuf::new(),
        };
        let mut expander =
            crate::expander::Expander::new(vec![item], crate::config::TriggerMode::Immediate);
        for ch in ";form ".chars() {
            assert!(expander.push_char(ch).is_none());
        }
        expander.take_prompt().unwrap()
    }

    fn editing(c: &mut Controller, now: Instant) -> (String, String) {
        let session = output(c.handle(1, json!({"version":1,"type":"register"}), now, true))[0]
            ["session"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(c.admit(candidate(), now).is_empty());
        assert!(c.tick(now, false).is_empty());
        let request = output(c.tick(now, true));
        let id = request[0]["id"].as_str().unwrap().to_owned();
        assert!(c
            .handle(
                1,
                json!({"version":1,"type":"ack","session":session,"id":id}),
                now,
                true
            )
            .is_empty());
        (session, id)
    }

    #[test]
    fn correction_does_not_extend_completion_and_empty_is_submission() {
        let now = Instant::now();
        let mut c = Controller::new(PromptSettings {
            completion_timeout_ms: 100,
            ..Default::default()
        })
        .unwrap();
        let (session, id) = editing(&mut c, now);
        let invalid = c.handle(
            1,
            json!({"version":1,"type":"submit","session":session,"id":id,"answers":{}}),
            now + Duration::from_millis(90),
            true,
        );
        assert_eq!(output(invalid)[0]["type"], "validation_error");
        let effects = c.handle(1,json!({"version":1,"type":"submit","session":session,"id":id,"answers":{"name":{"type":"text","value":""}}}),now+Duration::from_millis(99),true);
        assert!(matches!(&effects[0],Effect::Prepare { text,.. } if text=="Hello  "));
        let result = output(c.tick(now + Duration::from_millis(100), true));
        assert_eq!(result[0]["code"], "completion_timeout");
        assert!(!c.busy());
        assert_eq!(
            output(c.handle(
                1,
                json!({"version":1,"type":"submit","session":session,"id":id,"answers":{}}),
                now + Duration::from_millis(101),
                true
            ))[0]["code"],
            "stale_request"
        );
    }

    #[test]
    fn closure_waits_for_all_keys_and_queued_cancellation_keeps_fence() {
        let now = Instant::now();
        let mut c = Controller::new(PromptSettings::default()).unwrap();
        let (session, id) = editing(&mut c, now);
        let effects = c.handle(1,json!({"version":1,"type":"submit","session":session,"id":id,"answers":{"name":{"type":"text","value":"SECRET $|$ {{literal}}"}}}),now,false);
        assert!(
            matches!(&effects[0],Effect::Prepare { text, .. } if text=="Hello SECRET $|$ {{literal}} ")
        );
        assert!(c
            .prepared(&id, Ok(PreparedPrompt(1)), now, false)
            .is_empty());
        assert_eq!(
            output(c.physical_key(false, now, true))[0]["type"],
            "accepted"
        );
        assert!(c
            .handle(
                1,
                json!({"version":1,"type":"closed","session":session,"id":id}),
                now,
                true
            )
            .is_empty());
        let effects = c.tick(now + Duration::from_millis(150), true);
        assert!(matches!(effects[0], Effect::Commit { .. }));
        let outcome = output(c.cancel("disabled"));
        assert_eq!(outcome[0]["outcome"], "cancelled");
        assert!(c.busy());
        assert!(c.admit(candidate(), now).is_empty());
        assert!(c
            .finished(
                &id,
                PromptOutcome::FailedBeforeMutation(
                    crate::prompt_injection::PromptError::Cancelled
                )
            )
            .is_empty());
        assert!(!c.busy());
    }

    #[test]
    fn old_connection_cannot_send_answers_and_reconnect_does_not_receive_old_result() {
        let now = Instant::now();
        let mut c = Controller::new(PromptSettings::default()).unwrap();
        let (session, id) = editing(&mut c, now);
        assert_eq!(
            output(c.handle(
                2,
                json!({"version":1,"type":"submit","session":session,"id":id,"answers":{}}),
                now,
                true
            ))[0]["code"],
            "stale_request"
        );
        let effects = c.disconnect(1);
        assert_eq!(output(effects)[0]["outcome"], "cancelled");
        let new_session = output(c.handle(2, json!({"version":1,"type":"register"}), now, true))[0]
            ["session"]
            .clone();
        assert_ne!(new_session, session);
        assert!(c.finished(&id, PromptOutcome::Issued).is_empty());
        assert_eq!(
            output(c.handle(
                2,
                json!({"version":1,"type":"ack","session":new_session,"id":id}),
                now,
                true
            ))[0]["code"],
            "stale_request"
        );
    }

    #[test]
    fn request_ack_deadline_is_not_renewed_by_handler_lease() {
        let now = Instant::now();
        let mut c = Controller::new(PromptSettings::default()).unwrap();
        let session = output(c.handle(1, json!({"version":1,"type":"register"}), now, true))[0]
            ["session"]
            .clone();
        c.admit(candidate(), now);
        c.tick(now, true);
        c.handle(
            1,
            json!({"version":1,"type":"renew","session":session}),
            now + Duration::from_millis(1999),
            true,
        );
        let effects = output(c.tick(now + Duration::from_millis(2000), true));
        assert_eq!(effects[0]["code"], "ack_timeout");
        assert!(!c.busy());
    }

    #[test]
    fn lease_expiry_checked_on_admission_and_reconnection_changes_session() {
        let now = Instant::now();
        let mut c = Controller::new(PromptSettings::default()).unwrap();
        let first = output(c.handle(1, json!({"version":1,"type":"register"}), now, true))[0]
            ["session"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            output(c.handle(2, json!({"version":1,"type":"register"}), now, true))[0]["code"],
            "busy"
        );
        let expiry = now + Duration::from_secs(15);
        assert_eq!(
            output(c.handle(
                1,
                json!({"version":1,"type":"renew","session":first}),
                expiry,
                true
            ))[0]["code"],
            "wrong_session"
        );
        let next = output(c.handle(2, json!({"version":1,"type":"register"}), expiry, true));
        assert_ne!(next[0]["session"], first);
    }

    #[test]
    fn malformed_secrets_are_not_reflected() {
        let mut c = Controller::new(PromptSettings::default()).unwrap();
        for wire in [
            json!({"version":1,"type":"SECRET_TYPE"}),
            json!({"version":1,"type":"register","SECRET_PROPERTY":"value"}),
        ] {
            let response = output(c.handle(1, wire, Instant::now(), true));
            assert_eq!(response[0]["code"], "invalid_message");
            assert!(!response[0].to_string().contains("SECRET"));
        }
    }
}
