//! An operator's local password: how it is hashed, and how it is checked
//! (`features/operator-auth-and-roles.md` phase 2).
//!
//! This is the **break-glass** path, and the spec ranks it last of the three for
//! a reason worth repeating here rather than leaving in the feature file: it is a
//! new credential store. A unit that distributes security keys should be
//! authenticating with one, and the reason to build this at all is that a
//! register whose only administrator's key is in a drawer at home is a register
//! nobody can administer.
//!
//! ## What is stored
//!
//! One column, `operators.password_phc`, holding a [PHC string]:
//!
//! ```text
//! $argon2id$v=19$m=19456,t=2,p=1$<salt b64>$<hash b64>
//! ```
//!
//! The password is not in it, is not anywhere else, and — per `AGENTS.md` §2 —
//! never reaches a log, an audit entry, an error message, a UI label or a panic.
//! A *failed* sign-in is audited with the reason and the attempt count and
//! nothing that was typed, not even a length, exactly as a failed database unlock
//! already is.
//!
//! The parameters travel **inside** the string rather than being applied from
//! these constants at verification time. That is what lets the cost be raised
//! later without invalidating every existing hash: [`verify`] reads the
//! parameters it was given, and [`hash`] writes today's.
//!
//! [PHC string]: https://github.com/P-H-C/phc-string-format/blob/master/phc-sf-spec.md
//!
//! ## The parameters, and whose decision they are
//!
//! **These are documented defaults, pending ESI ratification** — `AGENTS.md` §8
//! makes cipher and KDF parameters the ESI's call, exactly as the SQLCipher KDF
//! parameters are in `features/db-password-and-encryption.md`. They are not
//! approved, and this module does not claim they are.
//!
//! The source is the **OWASP Password Storage Cheat Sheet**, which lists
//! `Argon2id` with `m=19456` (19 MiB), `t=2`, `p=1` as a minimum configuration.
//! It was chosen over RFC 9106's first recommendation (`m=2 GiB, t=1, p=4`)
//! because this runs in a desktop GUI on whatever workstation the unit has: two
//! gibibytes of working memory per sign-in is not a cost a laptop can pay without
//! the application appearing to hang, and a parameter set that gets lowered by
//! the first person who tries it is worse than one chosen for the machine it runs
//! on. RFC 9106's *second* recommendation (`m=64 MiB, t=3, p=4`) is the obvious
//! upgrade if the ESI wants one, and costs a constant here plus a re-hash on next
//! sign-in.
//!
//! `p=1` is deliberate on top of that: parallelism above one buys nothing on a
//! single interactive sign-in, and the `argon2` crate's single-threaded
//! implementation would serialise the lanes anyway.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// Memory cost, in kibibytes. 19 MiB — OWASP's stated minimum for Argon2id.
pub const MEMORY_KIB: u32 = 19_456;

/// Time cost (passes).
pub const ITERATIONS: u32 = 2;

/// Lanes. One: a sign-in is not a batch job.
pub const PARALLELISM: u32 = 1;

/// Salt length in bytes, before base64. 16 is the `password-hash` recommendation
/// and the PHC format's own example.
pub const SALT_BYTES: usize = 16;

/// Derived key length in bytes.
pub const HASH_BYTES: usize = 32;

/// One line naming the parameters, for the screen that has to be honest about
/// them and for the feature file's gate.
pub fn parameter_summary() -> String {
    format!(
        "Argon2id m={MEMORY_KIB} KiB, t={ITERATIONS}, p={PARALLELISM}, {SALT_BYTES}-byte salt, \
         {HASH_BYTES}-byte key — OWASP's documented minimum, pending ESI ratification"
    )
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// Deliberately vague, and it carries no password material: the caller has
    /// nothing to print but this.
    #[error("the password hashing parameters were refused: {0}")]
    Parameters(String),
    #[error("no randomness for a salt: {0}")]
    Randomness(String),
    #[error("the stored credential is not a valid password hash")]
    Stored,
    #[error("hashing failed")]
    Hashing,
}

fn hasher() -> Result<Argon2<'static>, CredentialError> {
    let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, Some(HASH_BYTES))
        .map_err(|e| CredentialError::Parameters(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// Hash a password for storage. Returns a PHC string, never anything derived
/// from the password that could be reversed.
///
/// The salt comes from the OS CSPRNG through `getrandom`, the same source
/// `features/secrets-custody.md` fixes for every generated secret — a seeded PRNG
/// is not acceptable for anything that protects a credential, and a per-hash salt
/// is what stops one rainbow table answering for the whole register.
pub fn hash(password: &str) -> Result<String, CredentialError> {
    let mut salt = [0u8; SALT_BYTES];
    getrandom::fill(&mut salt).map_err(|e| CredentialError::Randomness(e.to_string()))?;
    let salt = SaltString::encode_b64(&salt).map_err(|_| CredentialError::Hashing)?;

    // The error is swallowed on purpose rather than wrapped: `argon2`'s
    // `password_hash::Error` has variants that quote the *input*, and this input
    // is a password (`AGENTS.md` §2 — never an error message).
    let phc = hasher()?
        .hash_password(password.as_bytes(), &salt)
        .map_err(|_| CredentialError::Hashing)?;
    Ok(phc.to_string())
}

/// Is this the password behind that stored hash?
///
/// Returns `Ok(false)` for a wrong password and `Err` only when the *stored*
/// value cannot be read as a hash at all — two different facts, and conflating
/// them would report a corrupted column as a wrong password and lock somebody out
/// of a register nothing was wrong with.
///
/// The parameters used are the ones in the stored string, not the constants
/// above, so raising the cost never invalidates an existing credential.
pub fn verify(password: &str, stored: &str) -> Result<bool, CredentialError> {
    let parsed = PasswordHash::new(stored).map_err(|_| CredentialError::Stored)?;
    // `Argon2::default()` supplies the algorithm implementation; every cost
    // parameter is read out of `parsed`.
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// The `(m, t, p)` a stored hash was made with.
///
/// Exists so the parameters can be *asserted* rather than assumed — a hash whose
/// cost silently fell back to the crate's defaults would verify perfectly and
/// protect much less — and so an operator screen can say what a credential
/// actually costs to attack.
pub fn parameters_of(stored: &str) -> Option<(u32, u32, u32)> {
    let parsed = PasswordHash::new(stored).ok()?;
    if parsed.algorithm.as_str() != "argon2id" {
        return None;
    }
    let params = Params::try_from(&parsed).ok()?;
    Some((params.m_cost(), params.t_cost(), params.p_cost()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's test, in the words it asks for: Argon2id with the agreed
    /// parameters, and no password anywhere near the output.
    #[test]
    fn a_password_is_hashed_with_argon2id_at_the_documented_parameters() {
        let phc = hash("correct horse battery staple").expect("hashing works");
        assert!(
            phc.starts_with("$argon2id$v=19$"),
            "the algorithm and version are part of the stored value: {phc}"
        );
        assert_eq!(
            parameters_of(&phc),
            Some((MEMORY_KIB, ITERATIONS, PARALLELISM)),
            "a hash that quietly fell back to the crate's defaults would verify \
             perfectly and protect much less"
        );
    }

    #[test]
    fn the_stored_value_contains_nothing_of_the_password() {
        let password = "zebra-mango-quilt-7";
        let phc = hash(password).expect("hashing works");
        assert!(!phc.contains(password));
        for word in ["zebra", "mango", "quilt"] {
            assert!(!phc.contains(word), "{phc}");
        }
    }

    #[test]
    fn the_right_password_verifies_and_a_wrong_one_does_not() {
        let phc = hash("correct horse battery staple").expect("hashing works");
        assert!(verify("correct horse battery staple", &phc).unwrap());
        assert!(!verify("correct horse battery stapl", &phc).unwrap());
        assert!(!verify("", &phc).unwrap());
    }

    /// Two operators who choose the same password must not have the same row, or
    /// the register itself tells an attacker which accounts to try together.
    #[test]
    fn the_same_password_hashes_differently_every_time() {
        let first = hash("correct horse battery staple").unwrap();
        let second = hash("correct horse battery staple").unwrap();
        assert_ne!(first, second);
        assert!(verify("correct horse battery staple", &first).unwrap());
        assert!(verify("correct horse battery staple", &second).unwrap());
    }

    /// A corrupted column is not a wrong password, and reporting it as one would
    /// lock somebody out of a register nothing was wrong with.
    #[test]
    fn an_unreadable_stored_value_is_told_apart_from_a_wrong_password() {
        assert!(matches!(
            verify("anything", "not a phc string"),
            Err(CredentialError::Stored)
        ));
        assert!(matches!(
            verify("anything", ""),
            Err(CredentialError::Stored)
        ));
    }

    /// Raising the cost must not invalidate every existing credential, which is
    /// the whole reason the parameters travel inside the string.
    #[test]
    fn a_hash_made_at_other_parameters_still_verifies() {
        let params = Params::new(8, 1, 1, Some(HASH_BYTES)).unwrap();
        let cheap = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt = SaltString::encode_b64(&[7u8; SALT_BYTES]).unwrap();
        let phc = cheap
            .hash_password(b"an older credential", &salt)
            .unwrap()
            .to_string();

        assert_eq!(parameters_of(&phc), Some((8, 1, 1)));
        assert!(verify("an older credential", &phc).unwrap());
        assert!(!verify("something else", &phc).unwrap());
    }

    #[test]
    fn the_parameter_summary_says_it_is_not_approved() {
        let summary = parameter_summary();
        assert!(summary.contains("Argon2id"));
        assert!(summary.contains(&MEMORY_KIB.to_string()));
        assert!(
            summary.contains("pending ESI ratification"),
            "the summary is what an operator reads; it must not imply approval"
        );
    }
}
