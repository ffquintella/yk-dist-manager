//! The database password: how good it has to be, and how slowly a wrong one may
//! be retried.
//!
//! `features/db-password-and-encryption.md` phases 3 and 6. Both live here rather
//! than in `src/ui/` for the usual reason — a throttle that can be got wrong is
//! not something to leave in untested paint code.
//!
//! ## What this password actually protects
//!
//! One SQLCipher key for one file, shared by everyone in the unit, guarding a
//! register of who carries which security token plus their names, e-mails and
//! units. That shapes the policy in two directions at once:
//!
//! * It is **not** a login. There is no account to lock out, no reset e-mail and
//!   no administrator — losing it loses the data, which is why the spec calls
//!   encryption optional and says the documentation must not oversell it.
//! * It is a **file** password. The threat is a copied file: a backup on a
//!   share, a sync client's conflict copy, a laptop. Nobody is typing guesses at
//!   a prompt in that scenario, they are running a cracker against the file — so
//!   length is what matters, and a composition rule that pushes people towards
//!   `Password1!` actively harms.
//!
//! Hence: a **length floor**, advice rather than mandatory character classes, and
//! throttling that exists to slow a person at the keyboard rather than to pretend
//! it stops an offline attack.
//!
//! ## Deliberately not a password strength library
//!
//! No dictionary, no entropy model, no `zxcvbn`. Those earn their keep where a
//! service is choosing whether to accept a signup at scale. Here there is one
//! password, chosen once by an operator who can be told plainly what makes it
//! weak — and a dependency that scores passwords is a dependency that has to be
//! kept current forever for a single text field.

use std::time::Duration;

/// The shortest password this tool will set.
///
/// Twelve, not eight. This is a file-encryption key with no rate limit once the
/// file is copied, and the difference between eight and twelve characters is the
/// difference between hours and years on commodity hardware.
pub const MIN_LENGTH: usize = 12;

/// The length at which the advice stops nagging.
pub const COMFORTABLE_LENGTH: usize = 20;

/// How many failures before the prompt starts slowing down.
pub const FREE_ATTEMPTS: u32 = 3;

/// The longest a wrong password will be made to wait.
///
/// Capped so a mistyped password never looks like a hung application. The point
/// is to make scripted guessing at the prompt pointless, and thirty seconds does
/// that; a longer delay only punishes the operator who fat-fingered it.
pub const MAX_DELAY: Duration = Duration::from_secs(30);

/// How strong the password looks, for the meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Strength {
    /// Refused: it does not meet the floor.
    TooWeak,
    Weak,
    Fair,
    Strong,
}

impl Strength {
    /// A word, so the meter is not a bar of colour with no text.
    pub fn label(&self) -> &'static str {
        match self {
            Strength::TooWeak => "too weak — refused",
            Strength::Weak => "weak",
            Strength::Fair => "fair",
            Strength::Strong => "strong",
        }
    }

    /// 0.0–1.0, for a progress bar.
    pub fn fraction(&self) -> f32 {
        match self {
            Strength::TooWeak => 0.1,
            Strength::Weak => 0.4,
            Strength::Fair => 0.7,
            Strength::Strong => 1.0,
        }
    }

    pub fn is_acceptable(&self) -> bool {
        *self != Strength::TooWeak
    }
}

/// What the meter says, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub strength: Strength,
    /// Why it is refused, when it is. Empty otherwise.
    pub refusals: Vec<String>,
    /// How to make it better. Advice, never a requirement.
    pub advice: Vec<String>,
}

impl Assessment {
    pub fn is_acceptable(&self) -> bool {
        self.strength.is_acceptable()
    }

    /// One line for a status bar or a tooltip.
    pub fn summary(&self) -> String {
        if let Some(first) = self.refusals.first() {
            return format!("{}: {first}", self.strength.label());
        }
        match self.advice.first() {
            Some(advice) => format!("{} — {advice}", self.strength.label()),
            None => self.strength.label().to_owned(),
        }
    }
}

/// Judge a candidate password.
pub fn assess(password: &str) -> Assessment {
    let mut refusals = Vec::new();
    let mut advice = Vec::new();

    // Counted in characters, not bytes: a passphrase with an accent in it is not
    // longer than it looks to the person typing it.
    let length = password.chars().count();

    if length == 0 {
        return Assessment {
            strength: Strength::TooWeak,
            refusals: vec!["a password is required".into()],
            advice: Vec::new(),
        };
    }
    if length < MIN_LENGTH {
        refusals.push(format!(
            "at least {MIN_LENGTH} characters — this one has {length}. A copied database file \
             can be attacked offline as fast as the hardware allows, so length is what protects it"
        ));
    }
    if password.trim() != password {
        // Not a refusal: it may be deliberate. But a leading space is the kind of
        // thing that gets lost copying between a password manager and a prompt.
        advice.push("it starts or ends with a space, which is easy to lose when copying it".into());
    }

    let classes = character_classes(password);
    let repeats_only = has_single_character(password);
    let sequential = looks_sequential(password);

    if repeats_only {
        refusals.push("it is the same character repeated".into());
    }
    if sequential {
        refusals.push("it is a keyboard or alphabet run, which is the first thing tried".into());
    }

    if !refusals.is_empty() {
        return Assessment {
            strength: Strength::TooWeak,
            refusals,
            advice,
        };
    }

    // Length dominates, because that is what an offline attack costs. Character
    // classes shift it by one step at most — enough to reward variety without
    // pushing anyone towards a short password decorated with punctuation.
    let strength = match (length, classes) {
        (l, _) if l >= COMFORTABLE_LENGTH + 8 => Strength::Strong,
        (l, c) if l >= COMFORTABLE_LENGTH && c >= 2 => Strength::Strong,
        (l, _) if l >= COMFORTABLE_LENGTH => Strength::Fair,
        (_, c) if c >= 3 => Strength::Fair,
        _ => Strength::Weak,
    };

    if length < COMFORTABLE_LENGTH {
        advice.push(format!(
            "a passphrase of {COMFORTABLE_LENGTH}+ characters is far harder to attack than a \
             short password with symbols in it — several unrelated words work well"
        ));
    }
    if classes < 2 && length < COMFORTABLE_LENGTH + 8 {
        advice.push("mixing in a digit or a second case would help a little".into());
    }

    Assessment {
        strength,
        refusals,
        advice,
    }
}

/// How many of lower / upper / digit / other are present.
fn character_classes(password: &str) -> usize {
    let mut lower = false;
    let mut upper = false;
    let mut digit = false;
    let mut other = false;
    for c in password.chars() {
        if c.is_lowercase() {
            lower = true;
        } else if c.is_uppercase() {
            upper = true;
        } else if c.is_numeric() {
            digit = true;
        } else {
            other = true;
        }
    }
    usize::from(lower) + usize::from(upper) + usize::from(digit) + usize::from(other)
}

fn has_single_character(password: &str) -> bool {
    let mut chars = password.chars();
    match chars.next() {
        Some(first) => chars.all(|c| c == first),
        None => false,
    }
}

/// Is the whole thing one ascending or descending run?
///
/// Catches `123456789012` and `abcdefghijkl`, which pass a length floor and are
/// the first things any list tries.
fn looks_sequential(password: &str) -> bool {
    let chars: Vec<char> = password.chars().collect();
    if chars.len() < 4 {
        return false;
    }
    let step = |a: char, b: char| (b as i32) - (a as i32);
    let first = step(chars[0], chars[1]);
    if first != 1 && first != -1 {
        return false;
    }
    chars.windows(2).all(|pair| step(pair[0], pair[1]) == first)
}

/// Slows a prompt down after repeated failures.
///
/// Deliberately **not** a lockout. There is no administrator to unlock it and no
/// second factor to fall back on, so a lockout on a shared register would mean
/// the unit cannot open its own register until a timer expires — a denial of
/// service anybody with the file could trigger. A growing delay makes scripted
/// guessing pointless while leaving the operator who mistyped it able to try
/// again.
///
/// The clock is a parameter throughout, so the behaviour is testable without
/// waiting.
#[derive(Debug, Clone, Default)]
pub struct Throttle {
    failures: u32,
}

impl Throttle {
    pub fn new() -> Self {
        Self::default()
    }

    /// How long to wait before the next attempt may be made.
    pub fn delay(&self) -> Duration {
        if self.failures <= FREE_ATTEMPTS {
            return Duration::ZERO;
        }
        // Doubling from one second: 1, 2, 4, 8, 16, then capped.
        let steps = self.failures - FREE_ATTEMPTS - 1;
        let seconds = 1u64 << steps.min(6);
        Duration::from_secs(seconds).min(MAX_DELAY)
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Record a wrong password.
    pub fn failed(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        self.delay()
    }

    /// A correct password clears the history.
    pub fn succeeded(&mut self) {
        self.failures = 0;
    }

    /// Is the prompt currently slowed?
    pub fn is_throttled(&self) -> bool {
        self.delay() > Duration::ZERO
    }

    /// What the operator is told. Never how many attempts remain, because there
    /// is no limit — saying "2 left" would imply a lockout that does not exist.
    pub fn message(&self) -> Option<String> {
        let delay = self.delay();
        if delay.is_zero() {
            return None;
        }
        Some(format!(
            "{} failed attempts — wait {} second(s) before trying again",
            self.failures,
            delay.as_secs()
        ))
    }

    /// The audit detail for a failure.
    ///
    /// No password material, not even a length: `features/db-password-and-encryption.md`
    /// is explicit that a failed unlock carries none. Note also that this entry
    /// often *cannot* be written — the database it failed to open is the one
    /// holding the audit table — so it goes to the log, and to the mirror when
    /// one is configured.
    pub fn audit_detail(&self) -> String {
        format!("consecutive_failures={}", self.failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_password_is_refused() {
        let assessment = assess("");
        assert_eq!(assessment.strength, Strength::TooWeak);
        assert!(!assessment.is_acceptable());
    }

    #[test]
    fn a_short_password_is_refused_and_the_reason_names_the_threat() {
        // The operator should understand *why* twelve rather than eight: the
        // file can be copied and attacked without a prompt in the way.
        let assessment = assess("hunter2!A");
        assert_eq!(assessment.strength, Strength::TooWeak);
        let reason = assessment.refusals.first().expect("a reason");
        assert!(reason.contains("12"), "{reason}");
        assert!(reason.contains("offline"), "{reason}");
    }

    #[test]
    fn a_long_passphrase_of_plain_words_is_strong() {
        // The behaviour that matters: this must beat a short password decorated
        // with symbols, or the policy pushes people towards the worse choice.
        let passphrase = assess("correct horse battery staple");
        let decorated = assess("P@ssw0rd!23x");
        assert_eq!(passphrase.strength, Strength::Strong);
        assert!(
            passphrase.strength > decorated.strength,
            "a passphrase must outrank a short decorated password"
        );
    }

    #[test]
    fn a_repeated_character_is_refused_however_long() {
        let assessment = assess(&"a".repeat(40));
        assert_eq!(assessment.strength, Strength::TooWeak);
        assert!(
            assessment.refusals.iter().any(|r| r.contains("repeated")),
            "{:?}",
            assessment.refusals
        );
    }

    #[test]
    fn an_alphabet_run_is_refused_even_at_full_length() {
        let assessment = assess("abcdefghijklmno");
        assert_eq!(assessment.strength, Strength::TooWeak);
        assert!(
            assessment.refusals.iter().any(|r| r.contains("run")),
            "{:?}",
            assessment.refusals
        );

        // But a passphrase that merely *contains* a short run is fine.
        assert!(assess("my abc garden gate").is_acceptable());
    }

    #[test]
    fn a_digit_cycle_is_scored_weak_rather_than_refused_and_that_is_the_known_limit() {
        // `123456789012345` is not a monotone run — it wraps at 9 to 0 — so the
        // sequential check does not catch it, and this test says so rather than
        // implying a strength model that is not there. It still scores Weak: one
        // character class and under the comfortable length.
        //
        // Catching patterns like this properly means a dictionary and a period
        // detector, which is the password-strength library this module
        // deliberately does not carry for one text field. The length floor is
        // what is actually doing the work.
        let assessment = assess("123456789012345");
        assert_eq!(assessment.strength, Strength::Weak);
        assert!(
            assessment.is_acceptable(),
            "documented limit: it passes, and the advice tells the operator to do better"
        );
        assert!(!assessment.advice.is_empty());
    }

    #[test]
    fn surrounding_whitespace_is_advised_against_but_not_refused() {
        // It may be deliberate, and refusing it would be the tool deciding it
        // knows better than the password manager that produced it.
        let assessment = assess(" a good long passphrase ");
        assert!(assessment.is_acceptable());
        assert!(
            assessment.advice.iter().any(|a| a.contains("space")),
            "{:?}",
            assessment.advice
        );
    }

    #[test]
    fn length_counts_characters_rather_than_bytes() {
        // Twelve accented characters is twelve characters to the person typing
        // them, whatever UTF-8 makes of it.
        let assessment = assess("ãéíóúãéíóúãé");
        assert!(
            assessment.is_acceptable(),
            "12 characters should pass: {:?}",
            assessment.refusals
        );
    }

    #[test]
    fn the_meter_reads_as_words_not_only_as_a_bar() {
        let labels: Vec<&str> = [
            Strength::TooWeak,
            Strength::Weak,
            Strength::Fair,
            Strength::Strong,
        ]
        .iter()
        .map(|s| s.label())
        .collect();
        assert_eq!(
            labels.len(),
            labels
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        assert!(Strength::Strong.fraction() > Strength::Weak.fraction());
    }

    #[test]
    fn the_summary_leads_with_the_refusal_when_there_is_one() {
        assert!(assess("short").summary().contains("too weak"));
        assert!(
            assess("correct horse battery staple")
                .summary()
                .contains("strong")
        );
    }

    #[test]
    fn the_first_few_attempts_are_not_slowed() {
        // An operator mistyping once should not be punished.
        let mut throttle = Throttle::new();
        for _ in 0..FREE_ATTEMPTS {
            assert_eq!(throttle.failed(), Duration::ZERO);
        }
        assert!(!throttle.is_throttled());
        assert!(throttle.message().is_none());
    }

    #[test]
    fn the_delay_grows_and_then_stops_growing() {
        let mut throttle = Throttle::new();
        for _ in 0..FREE_ATTEMPTS {
            throttle.failed();
        }
        let mut seen = Vec::new();
        for _ in 0..12 {
            seen.push(throttle.failed());
        }
        assert_eq!(seen[0], Duration::from_secs(1));
        assert_eq!(seen[1], Duration::from_secs(2));
        assert_eq!(seen[2], Duration::from_secs(4));
        assert!(
            seen.iter().all(|d| *d <= MAX_DELAY),
            "a mistyped password must never look like a hung application"
        );
        assert_eq!(*seen.last().unwrap(), MAX_DELAY);
    }

    #[test]
    fn a_correct_password_clears_the_history() {
        let mut throttle = Throttle::new();
        for _ in 0..6 {
            throttle.failed();
        }
        assert!(throttle.is_throttled());
        throttle.succeeded();
        assert!(!throttle.is_throttled());
        assert_eq!(throttle.failures(), 0);
    }

    #[test]
    fn the_message_never_implies_a_lockout_that_does_not_exist() {
        // There is no administrator to unlock a shared register, so promising a
        // limited number of attempts would be a lie — and a lockout would be a
        // denial of service anybody holding the file could trigger.
        let mut throttle = Throttle::new();
        for _ in 0..6 {
            throttle.failed();
        }
        let message = throttle.message().expect("throttled");
        assert!(message.contains("wait"), "{message}");
        assert!(!message.to_lowercase().contains("remaining"), "{message}");
        assert!(!message.to_lowercase().contains("locked"), "{message}");
    }

    #[test]
    fn the_audit_detail_carries_no_password_material() {
        let mut throttle = Throttle::new();
        throttle.failed();
        let detail = throttle.audit_detail();
        assert_eq!(detail, "consecutive_failures=1");
        // Not even a length: the spec is explicit that a failed unlock records
        // nothing about what was typed.
        assert!(!detail.contains("length"));
    }
}
