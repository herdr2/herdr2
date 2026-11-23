//! Append-only multi-turn conversation pool (`AiSessionPool`) for high KV-cache hit rate
//! across incremental calls sharing the same Run or Attempt scope.

use super::prompts::{estimate_tokens, format_op_user_message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Keep sessions warm inside typical 5-minute provider KV-cache windows (4.5 min).
pub const SESSION_TTL: Duration = Duration::from_secs(270);
pub const MAX_SESSION_TURNS: usize = 12;
pub const MAX_SESSION_TOKENS: u32 = 12_000;
pub const MAX_ACTIVE_SESSIONS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

/// Builds a lock-free message list sharing the byte-identical `system + session_seed` KV-cache prefix
/// without appending to or contending on a mutable session mutex.
pub fn build_seeded_stateless_messages(
    system_prompt: &str,
    seed_context: Option<&str>,
    user_content: &str,
) -> Vec<ChatMessage> {
    let mut msgs = vec![ChatMessage::system(system_prompt)];
    if let Some(seed) = seed_context.map(str::trim).filter(|s| !s.is_empty()) {
        let seed_msg = format_op_user_message("session_seed", "en", &json!({ "context": seed }));
        msgs.push(ChatMessage::user(seed_msg));
        msgs.push(ChatMessage::assistant("{\"ack\":true}"));
    }
    msgs.push(ChatMessage::user(user_content));
    msgs
}

pub struct SessionState {
    pub system_hash: u64,
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub turns: usize,
    pub est_tokens: u32,
    pub last_used: Instant,
}

impl SessionState {
    fn new() -> Self {
        Self {
            system_hash: 0,
            model: String::new(),
            messages: Vec::new(),
            turns: 0,
            est_tokens: 0,
            last_used: Instant::now(),
        }
    }

    pub fn is_valid_for(&self, system_hash: u64, model: &str, max_tokens: u32, now: Instant) -> bool {
        !self.messages.is_empty()
            && self.system_hash == system_hash
            && self.model == model
            && self.turns < MAX_SESSION_TURNS
            && self.est_tokens < max_tokens
            && now.saturating_duration_since(self.last_used) < SESSION_TTL
    }

    /// Prepares an append-only message list for the next turn without mutating state until committed.
    /// Returns `(messages_to_send, reused_existing_session, seeded_prefix_messages)`.
    pub fn prepare_turn(
        &mut self,
        system_prompt: &str,
        system_hash: u64,
        model: &str,
        max_tokens: u32,
        seed_context: Option<&str>,
        user_content: &str,
    ) -> (Vec<ChatMessage>, bool, Vec<ChatMessage>) {
        let now = Instant::now();
        let cap = max_tokens.clamp(2048, MAX_SESSION_TOKENS);
        if self.is_valid_for(system_hash, model, cap, now) {
            let mut msgs = self.messages.clone();
            msgs.push(ChatMessage::user(user_content));
            return (msgs, true, Vec::new());
        }

        // Start a fresh session with immutable system prompt + optional pinned Turn 0 context.
        let mut base = vec![ChatMessage::system(system_prompt)];
        if let Some(seed) = seed_context.map(str::trim).filter(|s| !s.is_empty()) {
            let seed_msg = format_op_user_message("session_seed", "en", &json!({ "context": seed }));
            base.push(ChatMessage::user(seed_msg));
            base.push(ChatMessage::assistant("{\"ack\":true}"));
        }
        let mut send = base.clone();
        send.push(ChatMessage::user(user_content));
        (send, false, base)
    }

    /// Commits a successful turn so future calls on this scope append onto the exact cached prefix.
    pub fn commit_turn(
        &mut self,
        system_hash: u64,
        model: &str,
        fresh_base: Vec<ChatMessage>,
        user_content: String,
        assistant_content: String,
    ) {
        if !fresh_base.is_empty() {
            self.system_hash = system_hash;
            self.model = model.to_string();
            self.est_tokens = fresh_base.iter().map(|m| estimate_tokens(&m.content)).sum();
            self.messages = fresh_base;
            self.turns = 0;
        }
        self.est_tokens = self
            .est_tokens
            .saturating_add(estimate_tokens(&user_content))
            .saturating_add(estimate_tokens(&assistant_content));
        self.messages.push(ChatMessage::user(user_content));
        self.messages.push(ChatMessage::assistant(assistant_content));
        self.turns += 1;
        self.last_used = Instant::now();
    }
}

struct PoolEntry {
    state: Arc<tokio::sync::Mutex<SessionState>>,
    touched: Instant,
}

#[derive(Default)]
pub struct AiSessionPool {
    slots: Mutex<HashMap<String, PoolEntry>>,
}

impl AiSessionPool {
    pub fn acquire_slot(&self, scope_key: &str) -> Arc<tokio::sync::Mutex<SessionState>> {
        let now = Instant::now();
        let mut map = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = map.get_mut(scope_key) {
            entry.touched = now;
            return entry.state.clone();
        }
        if map.len() >= MAX_ACTIVE_SESSIONS {
            map.retain(|_, v| now.saturating_duration_since(v.touched) < SESSION_TTL);
        }
        while map.len() >= MAX_ACTIVE_SESSIONS {
            let oldest = map.iter().min_by_key(|(_, v)| v.touched).map(|(k, _)| k.clone());
            let Some(k) = oldest else { break };
            map.remove(&k);
        }
        let state = Arc::new(tokio::sync::Mutex::new(SessionState::new()));
        map.insert(scope_key.to_string(), PoolEntry { state: state.clone(), touched: now });
        state
    }

    pub fn evict(&self, scope_key: &str) {
        self.slots.lock().unwrap_or_else(|p| p.into_inner()).remove(scope_key);
    }

    pub fn evict_prefix(&self, prefix: &str) {
        self.slots.lock().unwrap_or_else(|p| p.into_inner()).retain(|k, _| !k.starts_with(prefix));
    }

    pub fn clear_all(&self) {
        self.slots.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    pub fn active_count(&self) -> usize {
        let now = Instant::now();
        let map = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        map.values().filter(|v| now.saturating_duration_since(v.touched) < SESSION_TTL).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn appends_incrementally_and_resets_on_model_or_turn_limit() {
        let pool = AiSessionPool::default();
        let slot = pool.acquire_slot("drove:loop:att_1");
        let mut st = slot.lock().await;

        let (m1, reused1, base1) = st.prepare_turn("SYS", 42, "m1", 8000, Some("task spec"), "step 1..8");
        assert!(!reused1);
        assert_eq!(m1.len(), 4); // system + seed_user + ack + turn1
        st.commit_turn(42, "m1", base1, "step 1..8".into(), "{\"looping\":false}".into());

        let (m2, reused2, base2) = st.prepare_turn("SYS", 42, "m1", 8000, Some("task spec"), "step 9..12");
        assert!(reused2);
        assert!(base2.is_empty());
        assert_eq!(m2.len(), 6); // exact prefix preserved + delta turn
        assert_eq!(m2[0..4], m1[0..4]);
        st.commit_turn(42, "m1", base2, "step 9..12".into(), "{\"looping\":true}".into());

        // Switching model invalidates prefix cleanly.
        let (m3, reused3, _) = st.prepare_turn("SYS", 42, "m2", 8000, Some("task spec"), "step 13");
        assert!(!reused3);
        assert_eq!(m3.len(), 4);
    }
}
