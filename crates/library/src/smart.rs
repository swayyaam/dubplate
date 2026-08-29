//! Smart playlists: rules stored as JSON, resolved to tracks on demand.
//!
//! Every field and operator is an enum, and every value is a bound parameter.
//! Nothing a user types ever reaches the SQL text, so a rule cannot be made to
//! mean something the author did not intend.

use anyhow::{anyhow, Result};
use rusqlite::types::ToSql;
use serde::{Deserialize, Serialize};

use crate::db::Library;
use crate::model::{TrackRow, TRACK_COLUMNS, TRACK_JOINS};

/// What a rule can talk about. Each maps to one whitelisted SQL expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    PlayCount,
    SkipCount,
    LastPlayed,
    AddedAt,
    Year,
    DurationMs,
    Bpm,
    SampleRate,
    BitDepth,
    EffectiveBits,
    TranscodeScore,
    SpectralCutoff,
    Loved,
    IsLossy,
    Codec,
    MusicKey,
    Title,
    Artist,
    Album,
    /// Derived: analysed at all.
    Analysed,
    /// Derived: the container declares more bits than the audio uses.
    Padded,
}

impl Field {
    fn expression(self) -> &'static str {
        match self {
            Field::PlayCount => "t.play_count",
            Field::SkipCount => "t.skip_count",
            Field::LastPlayed => "t.last_played",
            Field::AddedAt => "t.added_at",
            Field::Year => "t.year",
            Field::DurationMs => "t.duration_ms",
            Field::Bpm => "t.bpm",
            Field::SampleRate => "t.sample_rate",
            Field::BitDepth => "t.bit_depth",
            Field::EffectiveBits => "t.effective_bits",
            Field::TranscodeScore => "t.transcode_score",
            Field::SpectralCutoff => "t.spectral_cutoff",
            Field::Loved => "t.loved",
            Field::IsLossy => "t.is_lossy",
            Field::Codec => "t.codec",
            Field::MusicKey => "t.music_key",
            Field::Title => "COALESCE(t.title, t.path)",
            Field::Artist => "COALESCE(ar.name, '')",
            Field::Album => "COALESCE(al.title, '')",
            // Derived fields are whole predicates rather than values, so they
            // are handled before this is reached.
            Field::Analysed => "t.analyzed_at",
            Field::Padded => "t.effective_bits",
        }
    }

    /// Fields that are a condition in themselves, with no value to compare.
    fn derived(self) -> Option<&'static str> {
        match self {
            Field::Analysed => Some("t.analyzed_at IS NOT NULL"),
            Field::Padded => Some(
                "(t.effective_bits IS NOT NULL AND t.bit_depth IS NOT NULL
                  AND t.effective_bits < t.bit_depth)",
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
    IsNull,
    IsNotNull,
    /// For `loved`, and for the derived predicates.
    IsTrue,
    IsFalse,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Number(f64),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Sort {
    #[default]
    AddedNewest,
    AddedOldest,
    MostPlayed,
    LeastPlayed,
    RecentlyPlayed,
    Title,
    Artist,
    Bpm,
    TranscodeScore,
    Random,
}

impl Sort {
    fn clause(self) -> &'static str {
        match self {
            Sort::AddedNewest => "t.added_at DESC",
            Sort::AddedOldest => "t.added_at ASC",
            Sort::MostPlayed => "t.play_count DESC, t.last_played DESC",
            Sort::LeastPlayed => "t.play_count ASC, t.added_at DESC",
            Sort::RecentlyPlayed => "t.last_played DESC",
            Sort::Title => "COALESCE(t.title, t.path) COLLATE NOCASE",
            Sort::Artist => "COALESCE(ar.name, '\u{ffff}') COLLATE NOCASE, t.album_id, t.track_no",
            Sort::Bpm => "t.bpm",
            Sort::TranscodeScore => "t.transcode_score DESC",
            Sort::Random => "RANDOM()",
        }
    }
}

fn default_limit() -> usize {
    500
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SmartRules {
    /// Every condition must hold.
    #[serde(default)]
    pub all: Vec<Condition>,
    /// At least one must hold, if any are given.
    #[serde(default)]
    pub any: Vec<Condition>,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for SmartRules {
    fn default() -> Self {
        Self {
            all: Vec::new(),
            any: Vec::new(),
            sort: Sort::default(),
            limit: default_limit(),
        }
    }
}

/// Turn one condition into SQL plus its bound parameter.
fn compile(condition: &Condition) -> Result<(String, Option<Box<dyn ToSql>>)> {
    if let Some(predicate) = condition.field.derived() {
        return Ok(match condition.op {
            Op::IsTrue => (predicate.to_string(), None),
            Op::IsFalse => (format!("NOT {predicate}"), None),
            _ => return Err(anyhow!("{:?} is a yes-or-no field", condition.field)),
        });
    }

    let column = condition.field.expression();
    let operator = match condition.op {
        Op::Eq => "=",
        Op::Ne => "!=",
        Op::Gt => ">",
        Op::Gte => ">=",
        Op::Lt => "<",
        Op::Lte => "<=",
        Op::Contains => {
            let Some(Value::Text(text)) = &condition.value else {
                return Err(anyhow!("`contains` needs some text to look for"));
            };
            // The pattern is bound, not interpolated: a rule containing a
            // percent sign is a search for a percent sign.
            return Ok((
                format!("{column} LIKE ?1 ESCAPE '\\'"),
                Some(Box::new(format!("%{}%", escape_like(text)))),
            ));
        }
        Op::IsNull => return Ok((format!("{column} IS NULL"), None)),
        Op::IsNotNull => return Ok((format!("{column} IS NOT NULL"), None)),
        Op::IsTrue => return Ok((format!("{column} = 1"), None)),
        Op::IsFalse => return Ok((format!("COALESCE({column}, 0) = 0"), None)),
    };

    let value: Box<dyn ToSql> = match &condition.value {
        Some(Value::Number(number)) => Box::new(*number),
        Some(Value::Text(text)) => Box::new(text.clone()),
        None => return Err(anyhow!("{:?} needs a value to compare against", condition.op)),
    };
    Ok((format!("{column} {operator} ?1"), Some(value)))
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Resolve a rule set to tracks.
pub fn resolve(library: &Library, rules: &SmartRules) -> Result<Vec<TrackRow>> {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();

    let mut push = |condition: &Condition, into: &mut Vec<String>| -> Result<()> {
        let (sql, param) = compile(condition)?;
        // Each condition is compiled with ?1; renumber as they are collected so
        // the parameters line up however many there are.
        let sql = match param {
            Some(param) => {
                params.push(param);
                sql.replace("?1", &format!("?{}", params.len()))
            }
            None => sql,
        };
        into.push(sql);
        Ok(())
    };

    let mut all = Vec::new();
    for condition in &rules.all {
        push(condition, &mut all)?;
    }
    let mut any = Vec::new();
    for condition in &rules.any {
        push(condition, &mut any)?;
    }

    if !all.is_empty() {
        clauses.push(all.join(" AND "));
    }
    if !any.is_empty() {
        clauses.push(format!("({})", any.join(" OR ")));
    }
    let where_clause = if clauses.is_empty() {
        "1 = 1".to_string()
    } else {
        clauses.join(" AND ")
    };

    let limit = rules.limit.clamp(1, 5000);
    let sql = format!(
        "SELECT {TRACK_COLUMNS} {TRACK_JOINS}
         WHERE {where_clause}
         ORDER BY {}
         LIMIT {limit}",
        rules.sort.clause()
    );

    let conn = library.connection();
    let mut stmt = conn.prepare(&sql)?;
    let bound: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(bound.as_slice(), TrackRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_contains_rule_binds_its_pattern_rather_than_pasting_it() {
        let (sql, param) = compile(&Condition {
            field: Field::Title,
            op: Op::Contains,
            value: Some(Value::Text("100% pure".into())),
        })
        .unwrap();
        assert!(sql.contains("LIKE ?1"), "{sql}");
        assert!(param.is_some());
        // The percent sign is escaped, so it means a percent sign rather than
        // "anything at all".
        assert_eq!(escape_like("100% pure"), "100\\% pure");
    }

    #[test]
    fn a_derived_field_compiles_to_a_whole_predicate() {
        let (sql, param) = compile(&Condition {
            field: Field::Padded,
            op: Op::IsTrue,
            value: None,
        })
        .unwrap();
        assert!(sql.contains("effective_bits < t.bit_depth"), "{sql}");
        assert!(param.is_none());
    }

    #[test]
    fn a_comparison_without_a_value_is_refused() {
        assert!(compile(&Condition {
            field: Field::PlayCount,
            op: Op::Gt,
            value: None,
        })
        .is_err());
    }

    #[test]
    fn a_derived_field_cannot_be_compared_to_a_number() {
        assert!(compile(&Condition {
            field: Field::Analysed,
            op: Op::Gt,
            value: Some(Value::Number(3.0)),
        })
        .is_err());
    }

    #[test]
    fn rules_round_trip_through_json() {
        let rules = SmartRules {
            all: vec![Condition {
                field: Field::TranscodeScore,
                op: Op::Gte,
                value: Some(Value::Number(0.5)),
            }],
            any: Vec::new(),
            sort: Sort::TranscodeScore,
            limit: 50,
        };
        let json = serde_json::to_string(&rules).unwrap();
        let back: SmartRules = serde_json::from_str(&json).unwrap();
        assert_eq!(back.limit, 50);
        assert_eq!(back.sort, Sort::TranscodeScore);
        assert_eq!(back.all[0].field, Field::TranscodeScore);
    }
}
