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

/// American Soundex: first letter plus three consonant-class digits, so words that sound alike share a code.
fn soundex(word: &str) -> Option<String> {
    let class = |c: char| match c { 'b' | 'f' | 'p' | 'v' => '1', 'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2', 'd' | 't' => '3', 'l' => '4', 'm' | 'n' => '5', 'r' => '6', 'h' | 'w' => '-', _ => '0' };
    let mut letters = word.chars().filter(char::is_ascii_alphabetic).map(|c| c.to_ascii_lowercase());
    let first = letters.next()?;
    let (mut code, mut last) = (first.to_ascii_uppercase().to_string(), class(first));
    for c in letters {
        let digit = class(c);
        if digit == '-' { continue; }
        if digit != '0' && digit != last { code.push(digit); }
        last = digit;
    }
    Some(format!("{code:0<4}").chars().take(4).collect())
}

/// True when typed text names Wallis as a whole word, under any common spelling.
pub fn names_wallis(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|word| ["wallis", "wallace", "wally", "wallie"].contains(&word.to_lowercase().as_str()))
}

/// True when a speech transcript may name Wallis: a word, or a word split before a vowel ("wall is", "wall ace"), sounding like "Wallis".
/// ponytail: deliberately over-matches (walls, wells); the realtime agent reads the transcript by sound and calls `skip` when it was not meant.
pub fn sounds_like_wallis(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    let target = soundex("wallis");
    words.iter().map(|word| word.to_string()).chain(words.windows(2).filter(|pair| pair[1].starts_with(['a', 'e', 'i', 'o', 'u'])).map(|pair| pair.concat())).any(|word| soundex(&word) == target) || names_wallis(text)
}

/// True when an utterance heard over Wallis should cut him off: his name, "stop"/"wait", or two or more words
/// that are not all backchannel ("yeah", "mhm", "uh huh"). A lone acknowledgement lets him keep talking.
pub fn interrupts(text: &str) -> bool {
    let words: Vec<String> = text.split(|c: char| !(c.is_alphanumeric() || c == '\'')).filter(|w| !w.is_empty()).map(str::to_lowercase).collect();
    let backchannel = |w: &String| ["yeah", "yep", "yes", "ya", "yup", "mhm", "mm", "hmm", "uh", "huh", "um", "ok", "okay", "right", "sure", "lol", "haha", "ha", "nice", "cool", "true", "oh", "ah"].contains(&w.as_str());
    sounds_like_wallis(text) || words.iter().any(|w| w == "stop" || w == "wait") || (words.len() >= 2 && !words.iter().all(backchannel))
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

    #[test]
    fn transcripts_match_the_name_by_sound() {
        assert_eq!((soundex("Wallis"), soundex("Robert"), soundex("Ashcraft")), (Some("W420".into()), Some("R163".into()), Some("A261".into())));
        assert!(sounds_like_wallis("hey wallace") && sounds_like_wallis("Wallis's turn") && sounds_like_wallis("hey wall is there") && sounds_like_wallis("ok wally"));
        assert!(!sounds_like_wallis("I will go") && !sounds_like_wallis("what is that"));
    }

    #[test]
    fn only_real_speech_interrupts() {
        assert!(!interrupts("") && !interrupts("Yeah.") && !interrupts("uh huh") && !interrupts("okay, right") && !interrupts("no"));
        assert!(interrupts("wait") && interrupts("Stop!") && interrupts("Wallis") && interrupts("no that's wrong") && interrupts("yeah but why"));
    }
}
