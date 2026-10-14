//! When Wallis may speak up. Ported from the Node bot's `shouldConsider`; same rules, same test cases.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode { Voice, Text }

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Participation { Mentions, Natural }

pub struct Turn<'a> { pub mode: Mode, pub user_id: &'a str, pub direct: bool, pub participation: Participation, pub speaking: bool }

pub struct State<'a> { pub pending: u32, pub next_consider_at_ms: u64, pub last_reply_at_ms: u64, pub last_reply_user: Option<&'a str> }

/// A voice follow-up from the person Wallis just answered skips the cooldown for this long.
const FOLLOW_UP_MS: u64 = 20_000;

pub fn should_consider(turn: &Turn, state: &State, now_ms: u64) -> bool {
    if turn.direct { return true; }
    if turn.participation != Participation::Natural { return false; }
    if turn.mode == Mode::Voice && turn.speaking { return false; }
    if turn.mode == Mode::Voice && state.last_reply_user == Some(turn.user_id) && now_ms.saturating_sub(state.last_reply_at_ms) < FOLLOW_UP_MS { return true; }
    state.pending == 0 && now_ms >= state.next_consider_at_ms
}

/// True when the text addresses Wallis by name (wallis, wallace, wally, wallie as a whole word).
pub fn names_wallis(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| ["wallis", "wallace", "wally", "wallie"].contains(&word.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_participation_permits_opportunities_and_follow_ups_without_a_name() {
        let turn = Turn { mode: Mode::Voice, user_id: "Jake", direct: false, participation: Participation::Natural, speaking: false };
        let state = State { pending: 0, next_consider_at_ms: 20_000, last_reply_at_ms: 0, last_reply_user: None };
        assert!(!should_consider(&turn, &state, 1000));
        assert!(should_consider(&turn, &state, 20_000));
        assert!(!should_consider(&turn, &State { pending: 1, ..state }, 20_000));
        assert!(!should_consider(&Turn { speaking: true, ..turn }, &state, 20_000));
        assert!(should_consider(&Turn { direct: true, participation: Participation::Mentions, ..turn }, &state, 1000));
        assert!(!should_consider(&Turn { participation: Participation::Mentions, ..turn }, &state, 20_000));
        let replied = State { last_reply_user: Some("Jake"), last_reply_at_ms: 500, ..state };
        assert!(should_consider(&turn, &replied, 1000));
        assert!(!should_consider(&Turn { mode: Mode::Text, ..turn }, &replied, 1000));
    }

    #[test]
    fn recognises_the_name_only_as_a_whole_word() {
        assert!(names_wallis("hey Wallis, what's up") && names_wallis("thanks wally!") && names_wallis("WALLACE"));
        assert!(!names_wallis("the wallisford road") && !names_wallis("small walls"));
    }
}
