//! Returning a plugged key to factory default
//! (`features/key-lifecycle-and-revocation.md` phase 5).
//!
//! This is the exit the rest of the tool already points at. The decision of
//! 2026-08-13 is that **there is no in-place re-bootstrap**: a key that carries a
//! PIN, a credential or a certificate is reset first, deliberately, and then
//! prepared as if new. `features/device-detection.md` phase 5 turns that into a
//! blocking pre-flight refusal whose message names the reset — so until this
//! module existed, the refusal named a way forward the application did not have.
//!
//! ## The rules, and where each is enforced
//!
//! 1. **No confirmation, no writes.** [`perform`] takes a [`Confirmation`], which
//!    cannot be built except by naming the serial and the applets that were on
//!    screen, and which is re-checked against the request. Same shape as
//!    [`crate::bootstrap::Confirmation`], for the same reason: a type nobody can
//!    forge beats a boolean somebody can forget to test.
//! 2. **What is destroyed is named before it runs.** [`plan`] answers in two
//!    voices — what a reset of that applet destroys *in general*, and what this
//!    key was *observed* to carry — because "every credential on the key" and
//!    "the certificate in slot 9c that Ana signs with" land differently.
//! 3. **No record, no write.** The opening audit entry is written before the
//!    first reset, and a failure to write it abandons the whole thing. A key
//!    returned to factory default with no record of who did it is precisely the
//!    event this register exists to hold.
//! 4. **One applet's failure does not stop the others.** A half-reset key is
//!    worse than a reset one: the operator asked for factory default, and
//!    stopping at the first refusal would leave them with a key that is neither.
//!    The one exception is a key that has been unplugged, where every later call
//!    would fail the same way and the failures would be noise.
//! 5. **Nothing here needs a secret.** A factory reset destroys the PIN rather
//!    than authenticating with it, which is why this path exists for a key whose
//!    PIN nobody remembers. No method below takes a [`crate::secret::Secret`],
//!    and there is nothing for an audit entry to leak.
//!
//! ## Which transport does which applet
//!
//! Two of the three are `ykman`, and AGENTS.md requires a fallback to be labelled
//! as one where the operator sees it — [`Route::fallback`] is that label, and it
//! carries the reason rather than an apology:
//!
//! | Applet | Transport | Why |
//! |---|---|---|
//! | FIDO2 | `ykman` | `ctap-hid-fido2` implements no `authenticatorReset`; there is nothing native to call |
//! | PIV | native, where this build and this session have it | `yubikey` exposes the whole sequence |
//! | OTP | `ykman` | the OTP config frames are not implemented natively (`native-device-transport.md` phase 4) |

use super::write::WriteError;
use super::{AppletStates, Transport, TransportChoice};

/// One resettable applet.
///
/// Ordered as they are reset, and the order is not cosmetic: **FIDO2 first**,
/// because the authenticator only accepts a reset within a few seconds of being
/// powered up, so it is the applet with a deadline. PIV and OTP will wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Applet {
    Fido2,
    Piv,
    Otp,
}

impl Applet {
    pub const ALL: [Applet; 3] = [Applet::Fido2, Applet::Piv, Applet::Otp];

    /// The stable name, for an audit entry and a settings file.
    pub fn slug(&self) -> &'static str {
        match self {
            Applet::Fido2 => "fido2",
            Applet::Piv => "piv",
            Applet::Otp => "otp",
        }
    }

    /// What the operator calls it.
    pub fn label(&self) -> &'static str {
        match self {
            Applet::Fido2 => "FIDO2",
            Applet::Piv => "PIV",
            Applet::Otp => "OTP",
        }
    }

    /// What a reset of this applet destroys, whatever is on the key.
    ///
    /// Written as losses rather than as operations, because that is the question
    /// the operator is actually answering: not "what will this command do" but
    /// "what stops working when I click".
    pub fn destroys(&self) -> &'static [&'static str] {
        match self {
            Applet::Fido2 => &[
                "every credential on the key — resident and not, FIDO2 and U2F. Any service \
                 the holder registered this key with stops accepting it, including services \
                 this tool never knew about",
                "the FIDO2 PIN, its retry counter and any minimum-length policy",
            ],
            Applet::Piv => &[
                "the private key and certificate in every PIV slot, 9c included. A key \
                 generated on the device cannot be recovered from anywhere, because it was \
                 never anywhere else — anything signed with it stays signed, and nothing \
                 more can be",
                "the PIN, the PUK and the management key, back to the applet's published \
                 factory defaults",
            ],
            Applet::Otp => &[
                "the configuration in each programmed slot. A Yubico OTP credential is \
                 also registered with a validation server, and deleting the slot does not \
                 tell the server",
                "the access code that write-protects a slot — but only if the slot can be \
                 deleted at all, which needs the code this tool does not hold",
            ],
        }
    }

    /// The one instruction the operator has to follow for this applet, if any.
    ///
    /// FIDO2 has one and it is easy to get wrong: CTAP requires the reset to
    /// arrive within seconds of power-up and to be confirmed by touch, so a key
    /// that has been sitting in the port all morning refuses. That refusal reads
    /// like a broken tool unless somebody said so first — and telling the operator
    /// to win the race by hand is barely better, which is why
    /// [`crate::device::reinsert`] now runs it for them: confirm first, then the
    /// panel asks for the key and sends the reset the moment it is back.
    pub fn instruction(&self) -> Option<&'static str> {
        match self {
            Applet::Fido2 => Some(
                "after you confirm, this screen asks you to pull the key out and plug it \
                 back in, and sends the reset the moment it sees the key again — then touch \
                 it when it blinks. The authenticator refuses a reset that does not arrive \
                 within a few seconds of power-up, so nothing is written until the key is \
                 back in the port",
            ),
            Applet::Piv => None,
            Applet::Otp => None,
        }
    }
}

/// Which transport performs one applet's reset, and whether it is a fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub applet: Applet,
    /// `native` or `ykman`, matching [`Transport::label`].
    pub transport: &'static str,
    /// True when `ykman` is standing in for a native path that does not exist.
    /// AGENTS.md requires this to be visible in the plan the operator sees.
    pub fallback: bool,
    /// Why this transport, in one sentence, for the same plan.
    pub reason: &'static str,
}

/// Decide the transport for one applet.
///
/// Pure — the session's choice is handed in — so every branch is a unit test
/// rather than something that only shows up with a key in a port.
pub fn route(applet: Applet, choice: &TransportChoice) -> Route {
    let native_piv = cfg!(feature = "native-piv")
        && applet == Applet::Piv
        && choice.transport == Transport::Native;

    match (applet, native_piv) {
        (Applet::Piv, true) => Route {
            applet,
            transport: Transport::Native.label(),
            fallback: false,
            reason: "the `yubikey` crate performs the whole sequence in process",
        },
        (Applet::Piv, false) => Route {
            applet,
            transport: Transport::Ykman.label(),
            fallback: true,
            reason: "this session does not read through the native transport, so the PIV reset \
                     goes through `ykman piv reset`",
        },
        (Applet::Fido2, _) => Route {
            applet,
            transport: Transport::Ykman.label(),
            fallback: true,
            reason: "no crate in this build implements the CTAP `authenticatorReset` command — \
                     `ykman fido reset` is the only way to send it",
        },
        (Applet::Otp, _) => Route {
            applet,
            transport: Transport::Ykman.label(),
            fallback: true,
            reason: "the OTP configuration frames are not implemented natively \
                     (`features/native-device-transport.md` phase 4)",
        },
    }
}

/// One applet's entry in the preview: how it will be reset, and what that costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub applet: Applet,
    pub route: Route,
    /// What a reset of this applet destroys, whatever is on the key.
    pub destroys: Vec<String>,
    /// What *this* key was seen to carry. Empty when the applet read clean;
    /// a single "not read" line when it did not answer at all.
    pub observed: Vec<String>,
}

impl PlanItem {
    /// Did the applet answer when it was read?
    ///
    /// A reset of an applet nobody could read is allowed — it is the recovery
    /// path for a key in an unknown state, which is when it is needed most — but
    /// the preview must not present silence as "there is nothing on it".
    pub fn was_read(&self) -> bool {
        !self.observed.iter().any(|line| line.contains("not read"))
    }
}

/// Build the preview for a set of applets on one key.
///
/// `observed` is the read-only snapshot from [`crate::device::applets::read`].
/// Nothing here writes, and nothing here decides: the operator does.
pub fn plan(
    applets: &[Applet],
    observed: &AppletStates,
    choice: &TransportChoice,
) -> Vec<PlanItem> {
    Applet::ALL
        .iter()
        .filter(|applet| applets.contains(applet))
        .map(|&applet| PlanItem {
            applet,
            route: route(applet, choice),
            destroys: applet.destroys().iter().map(|s| (*s).to_owned()).collect(),
            observed: observed_state(applet, observed),
        })
        .collect()
}

/// What one applet was seen to hold, in the operator's words.
fn observed_state(applet: Applet, observed: &AppletStates) -> Vec<String> {
    let mut lines = Vec::new();
    match applet {
        Applet::Fido2 => match &observed.fido2 {
            None => lines.push(
                "FIDO2 was not read, so what is on it is unknown — the reset destroys it \
                 either way"
                    .to_owned(),
            ),
            Some(state) => {
                if state.pin_set {
                    lines
                        .push("a FIDO2 PIN is set, and will be destroyed with the rest".to_owned());
                }
                if let Some(length) = state.min_pin_length {
                    lines.push(format!(
                        "a minimum PIN length of {length} is in force, and goes back to the \
                         firmware's own minimum"
                    ));
                }
                if state.force_pin_change_set {
                    lines.push(
                        "a forced PIN change is pending, so this key was prepared and not yet \
                         collected"
                            .to_owned(),
                    );
                }
            }
        },
        Applet::Piv => match &observed.piv {
            None => lines.push(
                "PIV was not read, so what is in its slots is unknown — the reset destroys \
                 them either way"
                    .to_owned(),
            ),
            Some(state) => {
                // The attestation slot is excluded: it is factory-programmed on every
                // key and a PIV reset does not clear it, so naming it here would
                // describe a loss that will not happen.
                let slots = state.configured_slots();
                if !slots.is_empty() {
                    lines.push(format!(
                        "slot(s) {} hold a certificate and the private key behind it",
                        slots.join(", ")
                    ));
                }
                if state.management_key_changed {
                    lines.push(
                        "the management key is not the factory default — this key is under \
                         somebody's management, which may not be this unit's"
                            .to_owned(),
                    );
                }
                if state.pin_changed_from_default {
                    lines.push("the PIN is not the factory default".to_owned());
                }
            }
        },
        Applet::Otp => match &observed.otp {
            None => lines.push(
                "OTP was not read, so what is in its slots is unknown — the reset clears \
                 them either way"
                    .to_owned(),
            ),
            Some(state) => {
                let programmed: Vec<&str> = [
                    state.slot_one_programmed.then_some("1"),
                    state.slot_two_programmed.then_some("2"),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !programmed.is_empty() {
                    lines.push(format!(
                        "slot {} is programmed, and the configuration in it is not readable \
                         back — once cleared it cannot be restored from here",
                        programmed.join(" and ")
                    ));
                }
            }
        },
    }
    lines
}

/// Proof that the operator saw what would be destroyed and agreed to it.
///
/// No `Default`, no public field, one constructor — and the applets are carried,
/// not just counted, because "reset the FIDO2 applet" and "reset the PIV applet"
/// are different agreements to give.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    serial: u32,
    applets: Vec<Applet>,
}

impl Confirmation {
    /// Record that the operator confirmed resetting `applets` on `serial`.
    ///
    /// Call this from the click handler of the confirmation panel, and nowhere
    /// else.
    pub fn given(serial: u32, applets: &[Applet]) -> Self {
        let mut applets = applets.to_vec();
        applets.sort_unstable();
        applets.dedup();
        Self { serial, applets }
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    pub fn applets(&self) -> &[Applet] {
        &self.applets
    }
}

/// What a reset run is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub serial: u32,
    /// In [`Applet::ALL`] order, whatever order the caller supplied.
    pub applets: Vec<Applet>,
    pub operator: String,
}

impl Request {
    pub fn new(serial: u32, applets: &[Applet], operator: &str) -> Self {
        Self {
            serial,
            applets: Applet::ALL
                .iter()
                .copied()
                .filter(|a| applets.contains(a))
                .collect(),
            operator: operator.to_owned(),
        }
    }
}

/// How one applet's reset ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// The applet is at factory default because this run put it there.
    Done,
    /// Nothing was written, and nothing needed to be.
    Skipped,
    Failed,
}

impl Status {
    pub fn slug(&self) -> &'static str {
        match self {
            Status::Done => "done",
            Status::Skipped => "skipped",
            Status::Failed => "failed",
        }
    }
}

/// One applet's result, for the screen and for the record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub applet: Applet,
    pub transport: &'static str,
    pub status: Status,
    /// Secret-free by construction: the transports below build it from operation
    /// names and typed errors, and no method on [`Resetter`] takes a secret.
    pub detail: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ResetError {
    #[error(
        "this reset was not confirmed: the operator agreed to {confirmed} on serial \
         {confirmed_serial}, and the request is {actual} on serial {actual_serial}"
    )]
    ConfirmationMismatch {
        confirmed_serial: u32,
        confirmed: String,
        actual_serial: u32,
        actual: String,
    },
    #[error("nothing was selected to reset")]
    NothingSelected,
    #[error("the reset could not be recorded, so nothing was written to the key: {0}")]
    NotRecordable(String),
}

/// Where the record goes.
///
/// An abstraction rather than a `&Store` for the same reason the executor has
/// one: the engine has to be drivable by a test with no database, and a failure
/// to record has to be a first-class outcome rather than something swallowed.
pub trait Recorder {
    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String>;
}

/// Performs the reset of one applet on one key.
///
/// Separate from [`crate::device::write::WriteBackend`] deliberately. Every
/// method there borrows a [`crate::secret::Secret`]; this one takes none, because
/// a factory reset destroys the credential rather than presenting it. A trait
/// that mixed the two would invite an implementation to ask for a PIN in the one
/// path that exists precisely for keys whose PIN is gone.
pub trait Resetter {
    /// Which transport this implementation would use, before it is asked to.
    ///
    /// On the trait rather than derived by the caller so that the transport in
    /// the audit entry is the one that ran, and not the one a screen predicted.
    fn route(&self, applet: Applet) -> Route;

    /// Return one applet to factory default.
    ///
    /// `Ok` carries the transport's own account of what it did — for the record,
    /// so "reset" is not the only thing the register can say afterwards.
    fn reset(&mut self, serial: u32, applet: Applet) -> Result<Done, WriteError>;
}

/// What a transport did, when it succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Done {
    /// False when the applet was already at factory default and nothing was
    /// written — an empty OTP slot pair is the case that actually happens.
    pub written: bool,
    pub detail: String,
}

impl Done {
    pub fn written(detail: impl Into<String>) -> Self {
        Self {
            written: true,
            detail: detail.into(),
        }
    }

    pub fn nothing_to_do(detail: impl Into<String>) -> Self {
        Self {
            written: false,
            detail: detail.into(),
        }
    }
}

/// Reset the requested applets, recording as it goes.
///
/// Returns one [`Outcome`] per applet attempted — including the ones that were
/// skipped because the key was unplugged part-way, which are reported as failures
/// rather than omitted. An applet missing from the result is an applet the caller
/// did not ask for, and nothing else.
pub fn perform(
    request: &Request,
    confirmation: &Confirmation,
    resetter: &mut dyn Resetter,
    recorder: &mut dyn Recorder,
) -> Result<Vec<Outcome>, ResetError> {
    if request.applets.is_empty() {
        return Err(ResetError::NothingSelected);
    }
    // Rule 1, re-checked against *this* request: a confirmation for another key,
    // or for a different set of applets, is not a confirmation for this one.
    if confirmation.serial != request.serial || confirmation.applets != request.applets {
        return Err(ResetError::ConfirmationMismatch {
            confirmed_serial: confirmation.serial,
            confirmed: describe(&confirmation.applets),
            actual_serial: request.serial,
            actual: describe(&request.applets),
        });
    }

    let target = format!("serial:{}", request.serial);

    // Rule 3: before the first write, and fatal if it fails.
    recorder
        .audit(
            "key.reset.started",
            &target,
            &format!(
                "applets={} operator={}",
                describe(&request.applets),
                request.operator
            ),
        )
        .map_err(ResetError::NotRecordable)?;

    let mut outcomes = Vec::with_capacity(request.applets.len());
    let mut detached = false;

    for &applet in &request.applets {
        let transport = resetter.route(applet).transport;
        if detached {
            outcomes.push(Outcome {
                applet,
                transport,
                status: Status::Failed,
                detail: "not attempted: the key was no longer attached".to_owned(),
            });
            recorder
                .audit(
                    "key.reset.failed",
                    &target,
                    &format!(
                        "applet={} reason=not attempted, the key was no longer attached",
                        applet.slug()
                    ),
                )
                .map_err(ResetError::NotRecordable)?;
            continue;
        }

        match resetter.reset(request.serial, applet) {
            Ok(done) => {
                let status = if done.written {
                    Status::Done
                } else {
                    Status::Skipped
                };
                // Only a reset that *wrote* is an applet reset. Recording one for
                // an OTP slot pair that was already empty would put an event in
                // the trail for something that did not happen.
                let event = if done.written {
                    "key.applet_reset"
                } else {
                    "key.reset.skipped"
                };
                recorder
                    .audit(
                        event,
                        &target,
                        &format!(
                            "applet={} transport={transport} detail={}",
                            applet.slug(),
                            done.detail
                        ),
                    )
                    .map_err(ResetError::NotRecordable)?;
                outcomes.push(Outcome {
                    applet,
                    transport,
                    status,
                    detail: done.detail,
                });
            }
            Err(error) => {
                // Rule 4: carry on, unless there is no key to carry on with.
                detached = error.is_fatal_to_the_run();
                recorder
                    .audit(
                        "key.reset.failed",
                        &target,
                        &format!(
                            "applet={} transport={transport} reason={}",
                            applet.slug(),
                            error.detail()
                        ),
                    )
                    .map_err(ResetError::NotRecordable)?;
                outcomes.push(Outcome {
                    applet,
                    transport,
                    status: Status::Failed,
                    detail: error.detail(),
                });
            }
        }
    }

    let done = outcomes.iter().filter(|o| o.status == Status::Done).count();
    let failed = outcomes
        .iter()
        .filter(|o| o.status == Status::Failed)
        .count();
    let skipped = outcomes
        .iter()
        .filter(|o| o.status == Status::Skipped)
        .count();
    recorder
        .audit(
            "key.reset.finished",
            &target,
            &format!("reset={done} failed={failed} skipped={skipped}"),
        )
        .map_err(ResetError::NotRecordable)?;

    Ok(outcomes)
}

/// `fido2+piv+otp`, for an audit detail and a mismatch message.
pub fn describe(applets: &[Applet]) -> String {
    if applets.is_empty() {
        return "none".to_owned();
    }
    applets
        .iter()
        .map(|a| a.slug())
        .collect::<Vec<_>>()
        .join("+")
}

/// Did every applet the operator asked for come back to factory default?
pub fn all_done(outcomes: &[Outcome]) -> bool {
    !outcomes.is_empty() && outcomes.iter().all(|o| o.status != Status::Failed)
}

// -------------------------------------------------------------- the hardware

/// The [`Resetter`] that talks to a key.
///
/// One per reset, holding the session's transport choice so that the route shown
/// in the preview and the route taken are the same decision made once.
pub struct HardwareResetter {
    serial: u32,
    choice: TransportChoice,
    ykman: super::YkmanBackend,
}

impl HardwareResetter {
    pub fn for_key(serial: u32, choice: &TransportChoice) -> Self {
        Self {
            serial,
            choice: choice.clone(),
            ykman: super::YkmanBackend::default(),
        }
    }

    /// Clear each programmed OTP slot.
    ///
    /// The applet *is* its two slots, so this reads which are programmed and
    /// deletes those. A read that fails is a failure rather than a guess: asking
    /// `ykman` to delete a slot it never saw would turn "the applet is disabled
    /// over USB" into "the reset did not work", and the two need different
    /// answers from the operator.
    fn reset_otp(&self) -> Result<Done, WriteError> {
        const OP: &str = "otp.reset";
        let state =
            super::ykman::otp_state(&self.ykman, self.serial).map_err(|e| from_device(OP, e))?;

        let slots: Vec<u8> = [
            state.slot_one_programmed.then_some(1u8),
            state.slot_two_programmed.then_some(2u8),
        ]
        .into_iter()
        .flatten()
        .collect();

        if slots.is_empty() {
            return Ok(Done::nothing_to_do(
                "both OTP slots were already empty, so nothing was written",
            ));
        }

        for slot in &slots {
            super::ykman::delete_otp_slot(&self.ykman, self.serial, *slot)
                .map_err(|e| from_device(OP, e))?;
        }

        Ok(Done::written(format!(
            "OTP slot {} cleared via `ykman otp delete`",
            slots
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(" and ")
        )))
    }

    #[cfg(feature = "native-piv")]
    fn reset_piv_native(&self, serial: u32) -> Result<Done, WriteError> {
        super::native_piv::NativePiv::for_key(serial)
            .reset_applet(serial)
            .map(Done::written)
    }
}

impl Resetter for HardwareResetter {
    fn route(&self, applet: Applet) -> Route {
        route(applet, &self.choice)
    }

    fn reset(&mut self, serial: u32, applet: Applet) -> Result<Done, WriteError> {
        // The serial is checked rather than trusted: this holds one key's
        // transport choice, and resetting a different key with it would be the
        // worst possible way to discover that.
        if serial != self.serial {
            return Err(WriteError::NotAttached(serial));
        }

        match applet {
            Applet::Fido2 => super::ykman::reset_fido2(&self.ykman, serial)
                .map(|_| {
                    Done::written(
                        "FIDO2 reset via `ykman fido reset`: every credential and the PIN are gone",
                    )
                })
                .map_err(|e| from_device("fido2.reset", e)),
            Applet::Piv => {
                #[cfg(feature = "native-piv")]
                if self.route(applet).transport == Transport::Native.label() {
                    return self.reset_piv_native(serial);
                }
                super::ykman::reset_piv(&self.ykman, serial)
                    .map(|_| {
                        Done::written(
                            "PIV reset via `ykman piv reset`: slots cleared, PIN, PUK and \
                             management key back to the applet's factory defaults",
                        )
                    })
                    .map_err(|e| from_device("piv.reset", e))
            }
            Applet::Otp => self.reset_otp(),
        }
    }
}

/// Turn a transport-level failure into the engine's typed one.
///
/// `ykman`'s own message is kept — it carries the stderr line, which is the only
/// account of what the applet said — and nothing about it can hold a secret,
/// because no reset command is given one.
fn from_device(operation: &'static str, error: super::DeviceError) -> WriteError {
    match error {
        super::DeviceError::NoDevice => WriteError::Detached { operation },
        other => WriteError::Failed {
            operation,
            reason: other.to_string(),
        },
    }
}

// ------------------------------------------------------------------- the mock

/// A resetter that records what it was asked to do and can be made to fail.
///
/// Lives here rather than in a test file because the behaviour suites and the
/// unit tests both need it, and a second copy would drift from the first.
#[derive(Debug, Default)]
pub struct MockResetter {
    attached: Option<u32>,
    calls: Vec<Applet>,
    failures: Vec<(Applet, WriteError)>,
    /// Applets that report nothing to do rather than a write.
    already_clean: Vec<Applet>,
}

impl MockResetter {
    pub fn attached(serial: u32) -> Self {
        Self {
            attached: Some(serial),
            ..Default::default()
        }
    }

    pub fn fail(mut self, applet: Applet, error: WriteError) -> Self {
        self.failures.push((applet, error));
        self
    }

    pub fn already_clean(mut self, applet: Applet) -> Self {
        self.already_clean.push(applet);
        self
    }

    pub fn calls(&self) -> &[Applet] {
        &self.calls
    }
}

impl Resetter for MockResetter {
    fn route(&self, applet: Applet) -> Route {
        Route {
            applet,
            transport: "mock",
            fallback: false,
            reason: "a test stands in for the hardware",
        }
    }

    fn reset(&mut self, serial: u32, applet: Applet) -> Result<Done, WriteError> {
        if self.attached != Some(serial) {
            return Err(WriteError::NotAttached(serial));
        }
        self.calls.push(applet);
        if let Some(index) = self.failures.iter().position(|(a, _)| *a == applet) {
            return Err(self.failures.remove(index).1);
        }
        if self.already_clean.contains(&applet) {
            return Ok(Done::nothing_to_do(format!(
                "{} was already at factory default",
                applet.label()
            )));
        }
        Ok(Done::written(format!(
            "{} returned to factory default",
            applet.label()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::write::{Fido2State, OtpState, PivState};

    /// A recorder that keeps what it was told, and can refuse.
    #[derive(Default)]
    struct Trail {
        entries: Vec<(String, String)>,
        refuse: bool,
    }

    impl Recorder for Trail {
        fn audit(&mut self, event: &str, _target: &str, detail: &str) -> Result<(), String> {
            if self.refuse {
                return Err("the register is read-only".into());
            }
            self.entries.push((event.to_owned(), detail.to_owned()));
            Ok(())
        }
    }

    impl Trail {
        fn events(&self) -> Vec<&str> {
            self.entries.iter().map(|(e, _)| e.as_str()).collect()
        }
    }

    fn choice(transport: Transport) -> TransportChoice {
        TransportChoice {
            transport,
            disabled: false,
            reason: "a test".into(),
        }
    }

    #[test]
    fn every_applet_names_what_it_destroys_and_has_its_own_words() {
        // The obligation from AGENTS.md §2: a destructive operation names what
        // will be lost before it runs. An applet with an empty list would satisfy
        // the type and not the rule.
        let slugs: std::collections::BTreeSet<&str> =
            Applet::ALL.iter().map(|a| a.slug()).collect();
        assert_eq!(slugs.len(), Applet::ALL.len());

        for applet in Applet::ALL {
            assert!(
                !applet.destroys().is_empty(),
                "{} names nothing it destroys",
                applet.label()
            );
        }
        assert!(
            Applet::Fido2
                .instruction()
                .is_some_and(|i| i.contains("touch")),
            "the FIDO2 timing window is the one thing an operator must be told"
        );
    }

    #[test]
    fn the_fido2_applet_is_reset_first_because_it_is_the_one_with_a_deadline() {
        assert_eq!(Applet::ALL[0], Applet::Fido2);

        // And the order is imposed on the request rather than trusted from the
        // caller, so a screen that lists the checkboxes differently cannot
        // reorder the hardware calls.
        let request = Request::new(20_423_633, &[Applet::Otp, Applet::Fido2], "ana");
        assert_eq!(request.applets, vec![Applet::Fido2, Applet::Otp]);
    }

    #[test]
    fn fido2_and_otp_are_labelled_as_the_fallback_they_are() {
        // AGENTS.md: `ykman` is allowed where nothing else exists, and must be
        // labelled as a fallback in the plan the operator sees.
        let native = choice(Transport::Native);
        for applet in [Applet::Fido2, Applet::Otp] {
            let route = route(applet, &native);
            assert!(route.fallback, "{} must be labelled", applet.label());
            assert_eq!(route.transport, "ykman");
            assert!(!route.reason.is_empty());
        }
    }

    #[test]
    fn piv_goes_native_when_the_session_does_and_says_so_when_it_does_not() {
        let ykman = route(Applet::Piv, &choice(Transport::Ykman));
        assert_eq!(ykman.transport, "ykman");
        assert!(ykman.fallback);
        assert!(ykman.reason.contains("ykman piv reset"), "{}", ykman.reason);

        let native = route(Applet::Piv, &choice(Transport::Native));
        if cfg!(feature = "native-piv") {
            assert_eq!(native.transport, "native");
            assert!(!native.fallback);
        } else {
            // A build without the transport must not claim it has one.
            assert_eq!(native.transport, "ykman");
            assert!(native.fallback);
        }
    }

    #[test]
    fn the_preview_names_the_certificate_this_key_actually_carries() {
        // Rule 2: the general loss and the observed one are different sentences,
        // and it is the second that changes an operator's mind.
        let observed = AppletStates {
            piv: Some(PivState {
                occupied_slots: vec!["9c".into()],
                management_key_changed: true,
                pin_changed_from_default: true,
                pin_retries: Some(3),
            }),
            fido2: Some(Fido2State {
                pin_set: true,
                ..Fido2State::default()
            }),
            otp: Some(OtpState::default()),
            unread: Vec::new(),
        };
        let items = plan(&Applet::ALL, &observed, &choice(Transport::Native));
        assert_eq!(items.len(), 3);

        let piv = items.iter().find(|i| i.applet == Applet::Piv).unwrap();
        assert!(piv.observed.iter().any(|l| l.contains("9c")), "{piv:?}");
        assert!(piv.was_read());

        let fido2 = items.iter().find(|i| i.applet == Applet::Fido2).unwrap();
        assert!(fido2.observed.iter().any(|l| l.contains("PIN is set")));

        // An applet with nothing on it says nothing, rather than inventing a loss.
        let otp = items.iter().find(|i| i.applet == Applet::Otp).unwrap();
        assert!(otp.observed.is_empty(), "{otp:?}");
        assert!(otp.was_read());
    }

    #[test]
    fn the_factory_attestation_certificate_is_not_previewed_as_a_loss() {
        // Slot f9 is on every key from the factory, and a PIV reset does not clear it.
        // Listing it as a certificate "and the private key behind it" would tell the
        // operator a reset destroys something it does not, on a key carrying nothing.
        let observed = AppletStates {
            piv: Some(PivState {
                occupied_slots: vec!["f9".into()],
                ..PivState::default()
            }),
            fido2: Some(Fido2State::default()),
            otp: Some(OtpState::default()),
            unread: Vec::new(),
        };
        let items = plan(&[Applet::Piv], &observed, &choice(Transport::Native));
        let piv = &items[0];
        assert!(
            piv.observed.is_empty(),
            "nothing on this key is lost to a reset: {:?}",
            piv.observed
        );
        assert!(piv.was_read(), "and the applet did answer");
    }

    #[test]
    fn an_applet_that_did_not_answer_is_never_previewed_as_empty() {
        // The same distinction the pre-flight rests on: silence is not "clean".
        // A key in an unknown state is exactly what a reset is for, so this must
        // not block — it must be *said*.
        let items = plan(
            &[Applet::Piv],
            &AppletStates::default(),
            &choice(Transport::Native),
        );
        let piv = &items[0];
        assert!(!piv.was_read());
        assert!(piv.observed[0].contains("unknown"), "{:?}", piv.observed[0]);
    }

    #[test]
    fn only_the_applets_asked_for_are_previewed() {
        let items = plan(
            &[Applet::Fido2],
            &AppletStates::default(),
            &choice(Transport::Native),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].applet, Applet::Fido2);
    }

    #[test]
    fn a_confirmation_for_other_applets_does_not_authorise_these() {
        // Rule 1. The failure this prevents: the operator ticks FIDO2, changes
        // their mind to PIV, and a stale confirmation carries the agreement over
        // to a slot with a certificate in it.
        let mut resetter = MockResetter::attached(20_423_633);
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &[Applet::Piv], "ana");
        let confirmation = Confirmation::given(20_423_633, &[Applet::Fido2]);

        let error =
            perform(&request, &confirmation, &mut resetter, &mut trail).expect_err("must refuse");
        assert!(matches!(error, ResetError::ConfirmationMismatch { .. }));
        assert!(resetter.calls().is_empty(), "nothing may be attempted");
        assert!(trail.events().is_empty(), "and nothing recorded");
    }

    #[test]
    fn a_confirmation_for_another_key_does_not_authorise_this_one() {
        let mut resetter = MockResetter::attached(20_423_633);
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(11_111_111, &Applet::ALL);

        assert!(perform(&request, &confirmation, &mut resetter, &mut trail).is_err());
        assert!(resetter.calls().is_empty());
    }

    #[test]
    fn the_order_the_operator_ticked_the_boxes_in_does_not_change_the_agreement() {
        // `Confirmation::given` and `Request::new` both normalise, so a panel
        // that hands the applets over in click order still authorises the run.
        let mut resetter = MockResetter::attached(20_423_633);
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &[Applet::Otp, Applet::Piv], "ana");
        let confirmation = Confirmation::given(20_423_633, &[Applet::Piv, Applet::Otp]);

        let outcomes = perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(resetter.calls(), &[Applet::Piv, Applet::Otp]);
    }

    #[test]
    fn nothing_is_written_when_the_opening_entry_cannot_be_recorded() {
        // Rule 3, and the strictest one: a key returned to factory default with
        // no record of who did it is the event this register exists to hold.
        let mut resetter = MockResetter::attached(20_423_633);
        let mut trail = Trail {
            refuse: true,
            ..Trail::default()
        };
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(20_423_633, &Applet::ALL);

        let error =
            perform(&request, &confirmation, &mut resetter, &mut trail).expect_err("must refuse");
        assert!(matches!(error, ResetError::NotRecordable(_)));
        assert!(resetter.calls().is_empty(), "nothing may reach the key");
    }

    #[test]
    fn every_applet_that_was_reset_leaves_its_own_entry() {
        let mut resetter = MockResetter::attached(20_423_633);
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(20_423_633, &Applet::ALL);

        let outcomes = perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        assert!(all_done(&outcomes));
        assert_eq!(
            trail.events(),
            vec![
                "key.reset.started",
                "key.applet_reset",
                "key.applet_reset",
                "key.applet_reset",
                "key.reset.finished",
            ]
        );
        // The entries name which applet, or three identical lines say nothing.
        let applets: Vec<&str> = trail
            .entries
            .iter()
            .filter(|(event, _)| event == "key.applet_reset")
            .map(|(_, detail)| detail.as_str())
            .collect();
        assert!(applets.iter().any(|d| d.contains("applet=fido2")));
        assert!(applets.iter().any(|d| d.contains("applet=piv")));
        assert!(applets.iter().any(|d| d.contains("applet=otp")));
    }

    #[test]
    fn an_applet_that_was_already_clean_is_not_recorded_as_a_reset() {
        // An empty OTP slot pair is the case that actually happens, and an
        // event for a write that did not happen is a false entry in an immutable
        // trail.
        let mut resetter = MockResetter::attached(20_423_633).already_clean(Applet::Otp);
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &[Applet::Otp], "ana");
        let confirmation = Confirmation::given(20_423_633, &[Applet::Otp]);

        let outcomes = perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        assert_eq!(outcomes[0].status, Status::Skipped);
        assert!(all_done(&outcomes), "nothing failed");
        assert_eq!(
            trail.events(),
            vec![
                "key.reset.started",
                "key.reset.skipped",
                "key.reset.finished"
            ]
        );
    }

    #[test]
    fn one_applet_refusing_does_not_stop_the_others() {
        // Rule 4. The operator asked for factory default; stopping at the first
        // refusal leaves them with a key that is neither configured nor clean.
        let mut resetter = MockResetter::attached(20_423_633).fail(
            Applet::Fido2,
            WriteError::Failed {
                operation: "fido2.reset",
                reason: "the key was not re-inserted in time".into(),
            },
        );
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(20_423_633, &Applet::ALL);

        let outcomes = perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        assert_eq!(outcomes[0].status, Status::Failed);
        assert_eq!(outcomes[1].status, Status::Done);
        assert_eq!(outcomes[2].status, Status::Done);
        assert!(!all_done(&outcomes));
        assert_eq!(
            resetter.calls().len(),
            3,
            "PIV and OTP were still attempted"
        );
        assert!(trail.events().contains(&"key.reset.failed"));
    }

    #[test]
    fn an_unplugged_key_stops_the_run_and_says_which_applets_never_ran() {
        // The one exception to rule 4: every later call would fail identically,
        // and a list of three identical transport errors tells the operator less
        // than one sentence saying the key is gone.
        let mut resetter = MockResetter::attached(20_423_633).fail(
            Applet::Fido2,
            WriteError::Detached {
                operation: "fido2.reset",
            },
        );
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(20_423_633, &Applet::ALL);

        let outcomes = perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        assert_eq!(
            outcomes.len(),
            3,
            "an applet never attempted is still reported"
        );
        assert!(outcomes.iter().all(|o| o.status == Status::Failed));
        assert!(outcomes[1].detail.contains("no longer attached"));
        assert_eq!(resetter.calls(), &[Applet::Fido2]);
    }

    #[test]
    fn an_empty_selection_is_refused_rather_than_recorded_as_a_reset() {
        let mut resetter = MockResetter::attached(20_423_633);
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &[], "ana");
        let confirmation = Confirmation::given(20_423_633, &[]);

        assert!(matches!(
            perform(&request, &confirmation, &mut resetter, &mut trail),
            Err(ResetError::NothingSelected)
        ));
        assert!(trail.events().is_empty());
    }

    #[test]
    fn no_detail_this_engine_writes_can_carry_a_secret() {
        // There is nothing to leak, and this is what says so: no method on
        // `Resetter` takes a `Secret`, so every detail is built from operation
        // names and typed errors.
        let mut resetter = MockResetter::attached(20_423_633).fail(
            Applet::Piv,
            WriteError::WrongSecret {
                applet: "PIV",
                retries_left: 2,
            },
        );
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(20_423_633, &Applet::ALL);

        perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        for (_, detail) in &trail.entries {
            assert!(!detail.contains("123456"), "{detail}");
            assert!(!detail.contains("12345678"), "{detail}");
        }
    }

    #[test]
    fn the_summary_counts_what_happened() {
        let mut resetter = MockResetter::attached(20_423_633)
            .already_clean(Applet::Otp)
            .fail(Applet::Piv, WriteError::Locked { applet: "PIV" });
        let mut trail = Trail::default();
        let request = Request::new(20_423_633, &Applet::ALL, "ana");
        let confirmation = Confirmation::given(20_423_633, &Applet::ALL);

        perform(&request, &confirmation, &mut resetter, &mut trail).unwrap();
        let (_, summary) = trail
            .entries
            .iter()
            .find(|(event, _)| event == "key.reset.finished")
            .unwrap();
        assert_eq!(summary, "reset=1 failed=1 skipped=1");
    }

    #[test]
    fn a_selection_is_described_in_a_form_an_auditor_can_read() {
        assert_eq!(describe(&Applet::ALL), "fido2+piv+otp");
        assert_eq!(describe(&[]), "none");
    }
}
