//! Searching, sorting and paging the tables an operator reads.
//!
//! This is the half of `features/gui-shell.md` phases 3 and 4 that is worth
//! testing, and it is deliberately **not** in `src/ui/`. That directory is
//! excluded from the coverage gate because painting is not unit tested, and
//! `AGENTS.md` §4 is explicit that the exclusion is a contract rather than an
//! amnesty: *if you cannot test something, it is in the wrong place.* A filter
//! that silently drops rows is exactly the kind of thing that must not sit in
//! untested code — an operator concluding "that key is not in the register"
//! because a search quietly missed it is a worse outcome than a table that never
//! had search at all.
//!
//! ## One query box, several fields
//!
//! The tables are read at a desk by somebody holding a key and talking to the
//! person receiving it. They will type a serial fragment, part of a name, or an
//! e-mail domain into one box; asking them to pick a field first is a modal maze
//! for no gain. So a query matches across every field a row displays, and
//! multiple words must **all** match (in any field, in any order) — which is how
//! `ana esi` finds Ana in the ESI unit without also finding everyone in ESI.

use crate::domain::{DistributionRecord, Holder, KeyStatus, YubiKeyRecord};

/// How many rows a page holds by default.
///
/// Large enough that a unit with one shipment never pages at all, small enough
/// that the scroll bar stays meaningful. Paging exists for the register that has
/// been running for two years, not for the one that started last week.
pub const PAGE_SIZE: usize = 50;

/// A search across whatever fields a row shows.
///
/// Case-insensitive, and every whitespace-separated word must match somewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query(String);

impl Query {
    pub fn new(raw: &str) -> Self {
        Self(raw.trim().to_lowercase())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Does every word of the query appear somewhere in `haystack`?
    fn matches_any(&self, haystack: &[&str]) -> bool {
        if self.0.is_empty() {
            return true;
        }
        let lowered: Vec<String> = haystack.iter().map(|h| h.to_lowercase()).collect();
        self.0
            .split_whitespace()
            .all(|word| lowered.iter().any(|field| field.contains(word)))
    }
}

/// Which column a table is sorted by.
///
/// Per table rather than one shared enum: the columns differ, and a shared
/// "column index" would silently mean a different thing on each screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeySort {
    #[default]
    Serial,
    Model,
    Status,
    /// Most recently touched first — the useful default after a shipment.
    Updated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HolderSort {
    #[default]
    Name,
    Email,
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistributionSort {
    /// Newest hand-over first, which is what the screen is usually opened for.
    #[default]
    Date,
    Serial,
    Holder,
    /// Outstanding first: the keys somebody still has.
    Returned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    #[default]
    Ascending,
    Descending,
}

impl Direction {
    pub fn toggled(self) -> Self {
        match self {
            Direction::Ascending => Direction::Descending,
            Direction::Descending => Direction::Ascending,
        }
    }

    /// The arrow a column header shows when it is the sorted one.
    ///
    /// Text, not colour: `features/gui-shell.md` phase 10 requires that nothing
    /// carries meaning by colour alone.
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::Ascending => "▲",
            Direction::Descending => "▼",
        }
    }
}

/// One page of a filtered, sorted table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page<T> {
    pub rows: Vec<T>,
    /// Rows that matched the query, before paging.
    pub matched: usize,
    /// Rows in the table before the query.
    pub total: usize,
    /// 0-based.
    pub page: usize,
    pub pages: usize,
}

impl<T> Page<T> {
    pub fn is_filtered(&self) -> bool {
        self.matched != self.total
    }

    /// One line for under the table, so a filtered or paged view never looks
    /// like the whole register.
    ///
    /// The failure this prevents: an operator searches, sees nothing, and
    /// concludes the key was never registered — when it was, three pages down or
    /// spelled differently.
    pub fn describe(&self, noun: &str) -> String {
        if self.total == 0 {
            return format!("no {noun} yet");
        }
        let scope = if self.is_filtered() {
            format!("{} of {} {noun}", self.matched, self.total)
        } else {
            format!("{} {noun}", self.total)
        };
        if self.pages > 1 {
            format!("{scope} — page {} of {}", self.page + 1, self.pages)
        } else {
            scope
        }
    }
}

/// Take one page out of a filtered, sorted set.
fn paginate<T>(rows: Vec<T>, total: usize, page: usize, page_size: usize) -> Page<T> {
    let matched = rows.len();
    let page_size = page_size.max(1);
    let pages = matched.div_ceil(page_size).max(1);
    // A filter that shrinks the set can leave the operator on a page that no
    // longer exists; clamping is kinder than showing an empty table.
    let page = page.min(pages - 1);
    let rows = rows
        .into_iter()
        .skip(page * page_size)
        .take(page_size)
        .collect();
    Page {
        rows,
        matched,
        total,
        page,
        pages,
    }
}

/// Filter, sort and page the inventory.
pub fn keys<'a>(
    all: &'a [YubiKeyRecord],
    query: &Query,
    status: Option<KeyStatus>,
    sort: KeySort,
    direction: Direction,
    page: usize,
) -> Page<&'a YubiKeyRecord> {
    let serials: Vec<String> = all.iter().map(|k| k.serial.to_string()).collect();
    let mut rows: Vec<&YubiKeyRecord> = all
        .iter()
        .enumerate()
        .filter(|(i, key)| {
            status.is_none_or(|wanted| key.status == wanted)
                && query.matches_any(&[
                    &serials[*i],
                    &key.model,
                    &key.firmware,
                    &key.form_factor,
                    &key.batch,
                    &key.notes,
                    key.status.label(),
                ])
        })
        .map(|(_, key)| key)
        .collect();

    rows.sort_by(|a, b| {
        let ordering = match sort {
            KeySort::Serial => a.serial.cmp(&b.serial),
            KeySort::Model => a.model.to_lowercase().cmp(&b.model.to_lowercase()),
            KeySort::Status => a.status.label().cmp(b.status.label()),
            KeySort::Updated => a.updated_at.cmp(&b.updated_at),
        };
        // Serial breaks every tie, so the order is total and a redraw cannot
        // shuffle rows under the operator's cursor.
        match direction {
            Direction::Ascending => ordering.then(a.serial.cmp(&b.serial)),
            Direction::Descending => ordering.reverse().then(a.serial.cmp(&b.serial)),
        }
    });

    paginate(rows, all.len(), page, PAGE_SIZE)
}

/// Filter, sort and page the holders.
pub fn holders<'a>(
    all: &'a [Holder],
    query: &Query,
    sort: HolderSort,
    direction: Direction,
    page: usize,
) -> Page<&'a Holder> {
    let mut rows: Vec<&Holder> = all
        .iter()
        .filter(|h| {
            query.matches_any(&[
                &h.full_name,
                &h.email,
                &h.unit,
                &h.registration,
                &h.identification_number,
            ])
        })
        .collect();

    rows.sort_by(|a, b| {
        let ordering = match sort {
            HolderSort::Name => a.full_name.to_lowercase().cmp(&b.full_name.to_lowercase()),
            HolderSort::Email => a.email.to_lowercase().cmp(&b.email.to_lowercase()),
            HolderSort::Unit => a.unit.to_lowercase().cmp(&b.unit.to_lowercase()),
        };
        let tie = a.email.to_lowercase().cmp(&b.email.to_lowercase());
        match direction {
            Direction::Ascending => ordering.then(tie),
            Direction::Descending => ordering.reverse().then(tie),
        }
    });

    paginate(rows, all.len(), page, PAGE_SIZE)
}

/// Filter, sort and page the hand-overs.
pub fn distributions<'a>(
    all: &'a [DistributionRecord],
    query: &Query,
    outstanding_only: bool,
    sort: DistributionSort,
    direction: Direction,
    page: usize,
) -> Page<&'a DistributionRecord> {
    let serials: Vec<String> = all.iter().map(|d| d.key_serial.to_string()).collect();
    let mut rows: Vec<&DistributionRecord> = all
        .iter()
        .enumerate()
        .filter(|(i, d)| {
            (!outstanding_only || d.returned_at.is_none())
                && query.matches_any(&[
                    &serials[*i],
                    &d.holder_display,
                    &d.distributed_by,
                    &d.receipt_ref,
                    d.method.label(),
                ])
        })
        .map(|(_, d)| d)
        .collect();

    rows.sort_by(|a, b| {
        let ordering = match sort {
            DistributionSort::Date => a.distributed_at.cmp(&b.distributed_at),
            DistributionSort::Serial => a.key_serial.cmp(&b.key_serial),
            DistributionSort::Holder => a
                .holder_display
                .to_lowercase()
                .cmp(&b.holder_display.to_lowercase()),
            // Outstanding (never returned) sorts before returned.
            DistributionSort::Returned => a.returned_at.is_some().cmp(&b.returned_at.is_some()),
        };
        let tie = a.distributed_at.cmp(&b.distributed_at);
        match direction {
            Direction::Ascending => ordering.then(tie),
            Direction::Descending => ordering.reverse().then(tie),
        }
    });

    paginate(rows, all.len(), page, PAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceInfo;

    fn key(serial: u32, model: &str, notes: &str) -> YubiKeyRecord {
        let mut record = YubiKeyRecord::from_device(&DeviceInfo {
            serial,
            model: model.into(),
            firmware: "5.4.3".into(),
            form_factor: "Keychain (USB-A)".into(),
            nfc: true,
            usb_applications: vec!["FIDO2".into()],
        });
        record.notes = notes.into();
        record
    }

    fn holder(name: &str, email: &str, unit: &str) -> Holder {
        Holder::new(name, email, unit, "").unwrap()
    }

    #[test]
    fn an_empty_query_keeps_everything() {
        let all = vec![key(1, "5 NFC", ""), key(2, "5C", "")];
        let page = keys(
            &all,
            &Query::new("  "),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 2);
        assert!(!page.is_filtered());
    }

    #[test]
    fn a_serial_fragment_finds_the_key() {
        // What an operator actually types: the last few digits off the engraving.
        let all = vec![key(20_423_633, "5 NFC", ""), key(36_668_917, "5C NFC", "")];
        let page = keys(
            &all,
            &Query::new("4236"),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].serial, 20_423_633);
    }

    #[test]
    fn every_word_must_match_but_they_may_be_in_different_fields() {
        // `ana esi` should find Ana in ESI without finding everyone in ESI.
        let all = vec![
            holder("Ana Silva", "ana@example.org", "ESI"),
            holder("Bruno Costa", "bruno@example.org", "ESI"),
            holder("Ana Souza", "ana.souza@example.org", "DCI"),
        ];
        let page = holders(
            &all,
            &Query::new("ana esi"),
            HolderSort::Name,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].full_name, "Ana Silva");
    }

    #[test]
    fn searching_ignores_case() {
        let all = vec![holder("Ana Silva", "ana@example.org", "ESI")];
        for q in ["ANA", "ana", "AnA sIlVa"] {
            assert_eq!(
                holders(
                    &all,
                    &Query::new(q),
                    HolderSort::Name,
                    Direction::Ascending,
                    0
                )
                .rows
                .len(),
                1,
                "query `{q}` should match"
            );
        }
    }

    #[test]
    fn the_operators_own_note_is_searchable() {
        // The one field no device supplies is the one an operator will search by.
        let all = vec![key(1, "5 NFC", "spare for reception"), key(2, "5C", "")];
        let page = keys(
            &all,
            &Query::new("reception"),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].serial, 1);
    }

    #[test]
    fn a_status_filter_and_a_query_combine() {
        let mut all = vec![key(1, "5 NFC", ""), key(2, "5 NFC", "")];
        all[1].status = KeyStatus::Retired;
        let page = keys(
            &all,
            &Query::new("5 NFC"),
            Some(KeyStatus::Retired),
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].serial, 2);
    }

    #[test]
    fn sorting_reverses_and_ties_break_on_a_stable_field() {
        // Two keys of the same model must not swap places between redraws, or a
        // row moves under the operator's cursor as they click it.
        let all = vec![key(3, "5 NFC", ""), key(1, "5 NFC", ""), key(2, "5C", "")];
        let ascending = keys(
            &all,
            &Query::default(),
            None,
            KeySort::Model,
            Direction::Ascending,
            0,
        );
        let serials: Vec<u32> = ascending.rows.iter().map(|k| k.serial).collect();
        assert_eq!(serials, vec![1, 3, 2], "5 NFC before 5C, then by serial");

        let descending = keys(
            &all,
            &Query::default(),
            None,
            KeySort::Model,
            Direction::Descending,
            0,
        );
        assert_eq!(descending.rows[0].serial, 2, "5C first");
    }

    #[test]
    fn paging_splits_the_set_and_reports_where_it_is() {
        let all: Vec<YubiKeyRecord> = (1..=120).map(|n| key(n, "5 NFC", "")).collect();
        let first = keys(
            &all,
            &Query::default(),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(first.rows.len(), PAGE_SIZE);
        assert_eq!(first.pages, 3);
        assert_eq!(first.rows[0].serial, 1);
        assert!(first.describe("keys").contains("page 1 of 3"));

        let last = keys(
            &all,
            &Query::default(),
            None,
            KeySort::Serial,
            Direction::Ascending,
            2,
        );
        assert_eq!(last.rows.len(), 20);
        assert_eq!(last.rows[0].serial, 101);
    }

    #[test]
    fn a_page_beyond_the_end_clamps_rather_than_showing_nothing() {
        // Typing into the search box while on page 3 must not leave the operator
        // staring at an empty table.
        let all: Vec<YubiKeyRecord> = (1..=120).map(|n| key(n, "5 NFC", "")).collect();
        let page = keys(
            &all,
            &Query::new("119"),
            None,
            KeySort::Serial,
            Direction::Ascending,
            2,
        );
        assert_eq!(page.page, 0);
        assert_eq!(page.rows.len(), 1);
    }

    #[test]
    fn a_filtered_table_says_so_rather_than_looking_like_the_whole_register() {
        let all: Vec<YubiKeyRecord> = (1..=10).map(|n| key(n, "5 NFC", "")).collect();
        let page = keys(
            &all,
            &Query::new("7"),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert!(page.is_filtered());
        assert_eq!(page.describe("keys"), "1 of 10 keys");

        let unfiltered = keys(
            &all,
            &Query::default(),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(unfiltered.describe("keys"), "10 keys");
    }

    #[test]
    fn an_empty_register_says_it_is_empty_rather_than_reporting_zero_of_zero() {
        let page = keys(
            &[],
            &Query::default(),
            None,
            KeySort::Serial,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.describe("keys"), "no keys yet");
    }

    #[test]
    fn outstanding_hand_overs_can_be_isolated() {
        use crate::domain::{DeliveryMethod, DistributionRecord};
        use uuid::Uuid;

        let record = |serial: u32, holder: &str, returned: bool| DistributionRecord {
            id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
            key_serial: serial,
            holder_id: Uuid::new_v4(),
            holder_display: holder.to_owned(),
            distributed_at: chrono::Utc::now(),
            distributed_by: "felipe".into(),
            method: DeliveryMethod::InPerson,
            receipt_ref: String::new(),
            bootstrap_run_id: None,
            returned_at: returned.then(chrono::Utc::now),
            returned_to: None,
            notes: String::new(),
        };
        let all = vec![
            record(20_423_633, "Ana Silva <ana@example.org>", true),
            record(36_668_917, "Bruno Costa <bruno@example.org>", false),
        ];

        let page = distributions(
            &all,
            &Query::default(),
            true,
            DistributionSort::Date,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].key_serial, 36_668_917);
        assert!(
            page.is_filtered(),
            "and the screen has to say it is filtered"
        );

        // And the holder is searchable by name, which is how the desk finds a
        // hand-over when the key is not in front of them.
        let page = distributions(
            &all,
            &Query::new("bruno"),
            false,
            DistributionSort::Date,
            Direction::Ascending,
            0,
        );
        assert_eq!(page.rows.len(), 1);
    }

    #[test]
    fn the_sort_arrow_is_text_not_colour() {
        // Phase 10: nothing carries meaning by colour alone.
        assert_ne!(Direction::Ascending.arrow(), Direction::Descending.arrow());
        assert!(!Direction::Ascending.arrow().is_empty());
        assert_eq!(Direction::Ascending.toggled(), Direction::Descending);
    }
}
