//! The one rule every harness parser follows when the agent's record does not
//! look the way this version expects.
//!
//! Agents change their records between releases, mostly by adding to them.
//! Refusing a whole session over one unfamiliar block would lose everything
//! the session recorded, which misrepresents far more than an omission does;
//! guessing at a shape this version has never seen could record something
//! the agent never said. So, for every adapter:
//!
//! * **An unknown major shape fails loudly** and writes nothing for that
//!   record: the record's version is checked against the adapter's
//!   [`FormatPin`] ([`FormatPin::check_major`]).
//! * **An unfamiliar block, part, step or item type** inside a well-formed
//!   container is kept as a placeholder that names its type, never its
//!   content ([`Drift::placeholder`]), and counted, so that the ingest report
//!   can say what was not recorded.
//! * **A record type that carries no conversation content** (bookkeeping) is
//!   skipped and counted ([`Drift::skip`]).
//! * **A line that is not a record at all** either fails the read or is
//!   skipped and counted, as the adapter's [`MalformedLine`] says.
//!
//! # Placeholders
//!
//! A placeholder is `[<type>]`. New adapters use
//! [`PlaceholderStyle::Sanitised`]: the type name is kept only when it is 1 to
//! 64 bytes of `[A-Za-z0-9_.:-]`, else it reads `unknown-type`, so that an
//! agent's record cannot put arbitrary text into a capture through a type
//! name. Claude Code keeps [`PlaceholderStyle::Verbatim`], its 0.7.x
//! formatting, byte for byte: its captures must not change, and the CLI
//! already sanitises the names it prints.
use crate::harness::HarnessId;
use std::collections::BTreeMap;
use thiserror::Error;

/// The record format an adapter's parser is pinned to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatPin {
    pub harness: HarnessId,
    /// The major versions the parser reads. Anything else fails loudly.
    pub supported_major: &'static [u64],
    /// The exact versions the fixtures were recorded from, for the docs and
    /// reports. Informational: a minor version not listed is still read.
    pub verified: &'static [&'static str],
}

/// Why a record's version was refused.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DriftError {
    #[error("unparsable version {found:?}")]
    UnparsableVersion { found: String },
    #[error("version {found} is of a major version this cvc does not read")]
    UnsupportedMajor {
        found: String,
        supported: &'static [u64],
    },
}

impl FormatPin {
    /// Whether `version` (`<major>[.<anything>]`) is of a supported major
    /// version.
    pub fn check_major(&self, version: &str) -> Result<(), DriftError> {
        let major = version
            .split('.')
            .next()
            .and_then(|part| part.parse::<u64>().ok())
            .ok_or_else(|| DriftError::UnparsableVersion {
                found: version.to_owned(),
            })?;
        if self.supported_major.contains(&major) {
            Ok(())
        } else {
            Err(DriftError::UnsupportedMajor {
                found: version.to_owned(),
                supported: self.supported_major,
            })
        }
    }
}

/// What a parser does with a line that is not a record at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedLine {
    /// The read fails and nothing of the window is recorded (Claude Code).
    Fail,
    /// The line is skipped and counted ([`Drift::malformed`]).
    SkipAndCount,
}

/// How a placeholder names its type (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderStyle {
    /// The name when it is 1 to 64 bytes of `[A-Za-z0-9_.:-]`, else
    /// `unknown-type`.
    Sanitised,
    /// The name exactly as the record spells it. Claude Code only.
    Verbatim,
}

/// The name a placeholder of an unfamiliar type stands for when its own
/// cannot be shown.
pub const UNKNOWN_TYPE: &str = "unknown-type";
/// The longest type name a sanitised placeholder keeps.
pub const MAX_TYPE_NAME_BYTES: usize = 64;

impl PlaceholderStyle {
    /// The name `type_name` is recorded and counted under.
    pub fn name(self, type_name: &str) -> String {
        match self {
            PlaceholderStyle::Verbatim => type_name.to_owned(),
            PlaceholderStyle::Sanitised => {
                let shown = !type_name.is_empty()
                    && type_name.len() <= MAX_TYPE_NAME_BYTES
                    && type_name.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-')
                    });
                if shown {
                    type_name.to_owned()
                } else {
                    UNKNOWN_TYPE.to_owned()
                }
            }
        }
    }

    /// The placeholder text for a block of type `type_name`: `[<name>]`.
    pub fn placeholder(self, type_name: &str) -> String {
        format!("[{}]", self.name(type_name))
    }
}

/// One adapter's instance of the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriftRule {
    pub pin: FormatPin,
    pub placeholders: PlaceholderStyle,
    pub malformed: MalformedLine,
}

/// What one window held that this version does not record in full.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Drift {
    /// Unfamiliar block, part, step or item types, each with how many
    /// placeholders stand for it.
    pub placeholders: BTreeMap<String, usize>,
    /// Record types skipped as bookkeeping, each with how many were.
    pub skipped: BTreeMap<String, usize>,
    /// Lines that were not records at all and were skipped.
    pub malformed_lines: usize,
}

impl Drift {
    /// Counts one placeholder of `type_name` and returns its text.
    pub fn placeholder(&mut self, style: PlaceholderStyle, type_name: &str) -> String {
        let name = style.name(type_name);
        let text = format!("[{name}]");
        *self.placeholders.entry(name).or_default() += 1;
        text
    }

    /// Counts one placeholder already made for `type_name` (a planner that
    /// formats its own, as Claude Code's does).
    pub fn count_placeholder(&mut self, style: PlaceholderStyle, type_name: &str) {
        *self.placeholders.entry(style.name(type_name)).or_default() += 1;
    }

    /// Counts one skipped record of type `record_type`. The name is counted
    /// sanitised, whatever the adapter's placeholder style.
    pub fn skip(&mut self, record_type: &str) {
        *self
            .skipped
            .entry(PlaceholderStyle::Sanitised.name(record_type))
            .or_default() += 1;
    }

    /// Counts one malformed line that was skipped.
    pub fn malformed(&mut self) {
        self.malformed_lines += 1;
    }

    /// Adds what `other` counted.
    pub fn merge(&mut self, other: &Drift) {
        for (name, count) in &other.placeholders {
            *self.placeholders.entry(name.clone()).or_default() += count;
        }
        for (name, count) in &other.skipped {
            *self.skipped.entry(name.clone()).or_default() += count;
        }
        self.malformed_lines += other.malformed_lines;
    }

    /// How many placeholders were counted, of every type.
    pub fn placeholder_count(&self) -> usize {
        self.placeholders.values().sum()
    }

    /// Whether nothing was counted.
    pub fn is_empty(&self) -> bool {
        self.placeholders.is_empty() && self.skipped.is_empty() && self.malformed_lines == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: FormatPin = FormatPin {
        harness: HarnessId::Codex,
        supported_major: &[0, 1],
        verified: &["0.150.0"],
    };

    #[test]
    fn only_a_supported_major_version_is_read() {
        assert_eq!(PIN.check_major("0.150.0"), Ok(()));
        assert_eq!(PIN.check_major("1"), Ok(()));
        assert_eq!(
            PIN.check_major("2.0.0"),
            Err(DriftError::UnsupportedMajor {
                found: "2.0.0".into(),
                supported: &[0, 1],
            })
        );
        for unparsable in ["", "x.1", "-1.0", " 1.0"] {
            assert!(
                matches!(
                    PIN.check_major(unparsable),
                    Err(DriftError::UnparsableVersion { .. })
                ),
                "{unparsable:?}"
            );
        }
    }

    #[test]
    fn a_sanitised_placeholder_names_a_plain_type_and_nothing_else() {
        let style = PlaceholderStyle::Sanitised;
        assert_eq!(style.placeholder("image_url"), "[image_url]");
        assert_eq!(style.placeholder("item:v2.x-y"), "[item:v2.x-y]");
        for unshown in ["", "a b", "ignore]all[previous", "\u{1b}[31m", "é"] {
            assert_eq!(style.placeholder(unshown), "[unknown-type]", "{unshown:?}");
        }
        assert_eq!(
            style.placeholder(&"a".repeat(64)),
            format!("[{}]", "a".repeat(64))
        );
        assert_eq!(style.placeholder(&"a".repeat(65)), "[unknown-type]");
    }

    #[test]
    fn a_verbatim_placeholder_is_the_type_as_spelled() {
        assert_eq!(PlaceholderStyle::Verbatim.placeholder("a b]"), "[a b]]");
    }

    #[test]
    fn drift_counts_what_it_is_told() {
        let mut drift = Drift::default();
        assert!(drift.is_empty());
        assert_eq!(
            drift.placeholder(PlaceholderStyle::Sanitised, "mystery"),
            "[mystery]"
        );
        drift.placeholder(PlaceholderStyle::Sanitised, "mystery");
        drift.placeholder(PlaceholderStyle::Sanitised, "x y");
        drift.skip("queue-operation");
        drift.skip("bad type");
        drift.malformed();
        assert_eq!(drift.placeholder_count(), 3);
        assert_eq!(drift.placeholders["mystery"], 2);
        assert_eq!(drift.placeholders[UNKNOWN_TYPE], 1);
        assert_eq!(drift.skipped["queue-operation"], 1);
        assert_eq!(drift.skipped[UNKNOWN_TYPE], 1);

        let mut total = Drift::default();
        total.merge(&drift);
        total.merge(&drift);
        assert_eq!(total.placeholders["mystery"], 4);
        assert_eq!(total.malformed_lines, 2);
    }
}
