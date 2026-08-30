//! Reading and writing the tags in the files themselves.
//!
//! This is the one module that changes a user's files, so it is the one module
//! where being careful matters more than being quick:
//!
//!   - **Writes go to a copy.** The original is copied beside itself, the copy
//!     is edited, and the copy is renamed over the original. Renaming within a
//!     directory is atomic on APFS, so an interrupted write leaves either the
//!     old file or the new one and never half of either. Editing a hundred
//!     megabyte WAV in place and losing power halfway is how people lose music.
//!   - **The index is updated by us, not by the watcher.** A tag write changes
//!     the file's first 64KB, which is what the content key hashes, so a naive
//!     rescan would decide the audio had been replaced and throw away the
//!     track's analysis. Recording the new `(mtime, size, content_key)` as part
//!     of the write means the next scan sees a file it already knows.
//!
//! WAV and AIFF get their tags written twice, in both of the competing
//! conventions those containers support. See `write_targets`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use lofty::config::{ParseOptions, ParsingMode, WriteOptions};
use lofty::file::TaggedFileExt;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag, TagType};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::db::Library;
use crate::scan;
use crate::undo::{self, Previous, PreviousField, UndoStore};

/// A tag field the editor can change.
///
/// Deliberately a closed set rather than free-form keys: every one of these
/// maps to a well-defined `ItemKey` in every container, so what is written is
/// predictable in other software rather than merely round-tripping here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    Title,
    Artist,
    AlbumArtist,
    Album,
    TrackNumber,
    TrackTotal,
    DiscNumber,
    DiscTotal,
    Year,
    Genre,
    Composer,
    Comment,
}

/// Every field, in the order an editor should show them.
pub const FIELDS: [Field; 12] = [
    Field::Title,
    Field::Artist,
    Field::Album,
    Field::AlbumArtist,
    Field::TrackNumber,
    Field::TrackTotal,
    Field::DiscNumber,
    Field::DiscTotal,
    Field::Year,
    Field::Genre,
    Field::Composer,
    Field::Comment,
];

impl Field {
    fn key(self) -> ItemKey {
        match self {
            Field::Title => ItemKey::TrackTitle,
            Field::Artist => ItemKey::TrackArtist,
            Field::AlbumArtist => ItemKey::AlbumArtist,
            Field::Album => ItemKey::AlbumTitle,
            Field::TrackNumber => ItemKey::TrackNumber,
            Field::TrackTotal => ItemKey::TrackTotal,
            Field::DiscNumber => ItemKey::DiscNumber,
            Field::DiscTotal => ItemKey::DiscTotal,
            Field::Year => ItemKey::Year,
            Field::Genre => ItemKey::Genre,
            Field::Composer => ItemKey::Composer,
            Field::Comment => ItemKey::Comment,
        }
    }

    /// True for fields that must parse as a number, so the editor can refuse
    /// nonsense before it reaches a file.
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Field::TrackNumber | Field::TrackTotal | Field::DiscNumber | Field::DiscTotal | Field::Year
        )
    }
}

/// One field across a selection of tracks.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldValue {
    pub field: Field,
    /// The shared value, or `None` if the selection disagrees or all are empty.
    pub value: Option<String>,
    /// True when the selected tracks do not all agree. The editor shows this as
    /// "multiple" and leaves the field alone unless it is edited.
    pub varies: bool,
}

/// What the editor is asking to change.
///
/// Only the fields listed here are touched. A `null` value clears the field,
/// which is different from not listing it at all -- that distinction is the
/// whole reason multi-track editing can work without flattening everything to
/// whatever the first track happened to have.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldChange {
    pub field: Field,
    pub value: Option<String>,
}

/// What to do with the embedded cover.
///
/// A path rather than the bytes: an image is a megabyte, and a megabyte of
/// JSON numbers over the IPC boundary to reach a file the process can simply
/// open is a waste of both ends.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "data")]
pub enum ArtworkChange {
    Set { path: String },
    Remove,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TagEdit {
    #[serde(default)]
    pub fields: Vec<FieldChange>,
    #[serde(default)]
    pub artwork: Option<ArtworkChange>,
}

impl TagEdit {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.artwork.is_none()
    }
}

/// What happened to one file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteOutcome {
    pub id: i64,
    pub path: String,
    /// `None` on success, the reason on failure. One unwritable file must not
    /// stop the other ninety-nine.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteReport {
    pub written: usize,
    pub failed: usize,
    pub outcomes: Vec<WriteOutcome>,
}

/// Read the current tag values for a selection, collapsing them to one form.
///
/// Read from the files rather than the index, because the editor edits files:
/// the index does not carry composer, comment or the total counts, and reading
/// what is actually there is the only way to avoid writing back a value the
/// editor never showed.
pub fn read_fields(library: &Library, ids: &[i64]) -> Result<Vec<FieldValue>> {
    let paths = paths_for(library, ids)?;
    let mut seen: BTreeMap<Field, Option<Option<String>>> = BTreeMap::new();

    for (_, path) in &paths {
        let values = read_one(Path::new(path)).unwrap_or_default();
        for field in FIELDS {
            let value = values.get(&field).cloned();
            match seen.get(&field) {
                // First file decides the candidate.
                None => {
                    seen.insert(field, Some(value));
                }
                // Already disagreed; nothing can bring it back.
                Some(None) => {}
                Some(Some(current)) => {
                    if *current != value {
                        seen.insert(field, None);
                    }
                }
            }
        }
    }

    Ok(FIELDS
        .iter()
        .map(|field| match seen.get(field) {
            Some(Some(value)) => FieldValue {
                field: *field,
                value: value.clone(),
                varies: false,
            },
            _ => FieldValue {
                field: *field,
                value: None,
                varies: seen.contains_key(field),
            },
        })
        .collect())
}

/// Every tag value in one file, for the fields the editor knows about.
pub fn read_one(path: &Path) -> Result<BTreeMap<Field, String>> {
    let tagged = probe(path)?;
    let mut out = BTreeMap::new();
    // The primary tag is what other software reads first, so it is what the
    // editor should show. Falling back to any tag at all is better than showing
    // an empty editor for a file that plainly has tags.
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(out);
    };
    for field in FIELDS {
        if let Some(value) = tag.get_string(field.key()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                out.insert(field, trimmed.to_owned());
            }
        }
    }
    Ok(out)
}

/// True when the file already carries embedded cover art.
pub fn has_artwork(path: &Path) -> bool {
    probe(path)
        .ok()
        .and_then(|tagged| {
            tagged
                .primary_tag()
                .or_else(|| tagged.first_tag())
                .map(|tag| !tag.pictures().is_empty())
        })
        .unwrap_or(false)
}

/// Apply one edit to many tracks, then bring the index back in step.
///
/// Each file is written independently: a failure is recorded and the rest
/// continue, because a selection of a hundred should not be abandoned because
/// one of them is read-only.
pub fn write(
    library: &mut Library,
    store: &UndoStore,
    ids: &[i64],
    edit: &TagEdit,
) -> Result<WriteReport> {
    let mut report = WriteReport::default();
    if edit.is_empty() {
        return Ok(report);
    }

    let cover = resolve_cover(&edit.artwork)?;
    let touched: Vec<Field> = edit.fields.iter().map(|change| change.field).collect();
    let paths = paths_for(library, ids)?;
    let mut updates = Vec::new();
    let mut previous = Vec::new();

    for (id, path) in paths {
        let file = Path::new(&path);
        // Captured before the write, kept only if the write succeeds: offering
        // to undo something that never happened is worse than offering nothing.
        let before = capture(file, &touched, cover.is_some());

        match write_one(file, edit, cover.as_ref()) {
            Ok(()) => {
                report.written += 1;
                updates.push((id, path.clone()));
                previous.push(Previous {
                    track_id: id,
                    path: path.clone(),
                    fields: before.0,
                    artwork: before.1,
                });
                report.outcomes.push(WriteOutcome { id, path, error: None });
            }
            Err(error) => {
                report.failed += 1;
                report.outcomes.push(WriteOutcome {
                    id,
                    path,
                    error: Some(format!("{error:#}")),
                });
            }
        }
    }

    resync(library, &updates)?;
    undo::record(library, store, "Edit tags", &previous)?;
    Ok(report)
}

/// What a file holds now, for the fields a write is about to change.
///
/// Only those fields: recording all twelve would make undo restore values the
/// operation never touched, which is not what undoing an operation means.
fn capture(
    path: &Path,
    fields: &[Field],
    artwork: bool,
) -> (Vec<PreviousField>, Option<Option<Vec<u8>>>) {
    let values = read_one(path).unwrap_or_default();
    let previous = fields
        .iter()
        .map(|field| PreviousField {
            field: *field,
            value: values.get(field).cloned(),
        })
        .collect();
    let cover = artwork.then(|| current_artwork(path));
    (previous, cover)
}

/// Put a recorded batch back.
///
/// The same write path in reverse, so it cannot drift from what writing does.
/// It restores tags only -- it never touches audio -- and it does not check
/// whether something else has edited the file since, because sequential undo
/// of several operations would then block itself.
pub fn undo_batch(
    library: &mut Library,
    store: &UndoStore,
    batch: i64,
) -> Result<WriteReport> {
    let mut report = WriteReport::default();
    let entries = undo::entries(library, batch)?;
    let mut updates = Vec::new();

    for entry in entries {
        let (id, path) = (entry.track_id, entry.path);
        let file = Path::new(&path);
        let edit = TagEdit {
            fields: entry
                .fields
                .into_iter()
                .map(|previous| FieldChange {
                    field: previous.field,
                    value: previous.value,
                })
                .collect(),
            artwork: None,
        };
        // -1 means the write never touched artwork, so undo must not either.
        let cover = match entry.had_art {
            1 => entry
                .art_blob
                .as_deref()
                .and_then(|hash| undo::artwork_for(store, hash))
                .map(Cover::Set),
            0 => Some(Cover::Remove),
            _ => None,
        };

        if edit.fields.is_empty() && cover.is_none() {
            continue;
        }
        match write_one(file, &edit, cover.as_ref()) {
            Ok(()) => {
                report.written += 1;
                updates.push((id, path.clone()));
                report.outcomes.push(WriteOutcome { id, path, error: None });
            }
            Err(error) => {
                report.failed += 1;
                report.outcomes.push(WriteOutcome {
                    id,
                    path,
                    error: Some(format!("{error:#}")),
                });
            }
        }
    }

    resync(library, &updates)?;
    // Dropped whether or not every file came back: a batch that half applied
    // cannot be offered again as if it were intact.
    undo::forget(library, store, batch)?;
    Ok(report)
}

/// An artwork change with its image already loaded.
///
/// Resolved once per operation rather than once per file: editing twelve
/// tracks should read the chosen cover once, and undo supplies bytes it has
/// rather than a path that may no longer exist.
#[derive(Debug, Clone)]
enum Cover {
    Set(Vec<u8>),
    Remove,
}

fn resolve_cover(change: &Option<ArtworkChange>) -> Result<Option<Cover>> {
    match change {
        None => Ok(None),
        Some(ArtworkChange::Remove) => Ok(Some(Cover::Remove)),
        Some(ArtworkChange::Set { path }) => {
            let bytes = std::fs::read(path)
                .with_context(|| format!("reading the image at {path}"))?;
            Ok(Some(Cover::Set(bytes)))
        }
    }
}

/// The cover a file currently carries, so a write can be undone.
fn current_artwork(path: &Path) -> Option<Vec<u8>> {
    let tagged = probe(path).ok()?;
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag())?;
    tag.pictures().first().map(|picture| picture.data().to_vec())
}

/// Write one file, through a copy, and rename it into place.
///
/// Each tag convention is saved in its own pass over the copy. Handing lofty a
/// file object carrying two tags and saving once looks tidier and quietly loses
/// one of them: on a WAV that already had an ID3v2 chunk, the RIFF INFO tag
/// went in as far as memory and no further. Saving them one at a time, each
/// against the file as the previous pass left it, is what actually produces
/// both chunks.
fn write_one(path: &Path, edit: &TagEdit, cover: Option<&Cover>) -> Result<()> {
    use lofty::tag::TagExt;

    let tagged = probe(path)?;
    let existing = tagged.primary_tag().or_else(|| tagged.first_tag()).cloned();

    let temp = temp_beside(path)?;
    // Copy first: saving a tag rewrites an existing file, so the target has to
    // already be this file, audio and all.
    std::fs::copy(path, &temp)
        .with_context(|| format!("copying {} before writing tags", path.display()))?;

    let outcome = (|| -> Result<()> {
        let target = write_target(&tagged);

        // A fresh tag carrying the file's values, never the parsed tag itself.
        //
        // Cloning what lofty read and saving it back produces a chunk that a
        // later save cannot replace: the second edit reports success and the
        // file keeps the first edit's values. Rebuilding the tag from its
        // fields sidesteps that, at the cost of dropping frames this module
        // does not model.
        let mut tag = seed(target, tagged.tag(target).or(existing.as_ref()));
        apply(&mut tag, edit, cover)?;
        tag.save_to_path(&temp, WriteOptions::default())
            .with_context(|| format!("writing {target:?} to {}", path.display()))?;

        // Nothing counts as written until it can be read back.
        anyhow::ensure!(
            verify(&temp, edit),
            "the tag writer reported success but {} did not change",
            path.display()
        );
        Ok(())
    })();

    if let Err(error) = outcome {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }

    std::fs::rename(&temp, path)
        .with_context(|| format!("replacing {} with the rewritten copy", path.display()))?;
    Ok(())
}

/// Read a file back and confirm the edit is actually in it.
///
/// Not paranoia: a tag save can report success and leave the previous values
/// in the file, which is the worst kind of failure here -- the editor would
/// show a change that never happened, and undo would record a state that never
/// existed. Nothing counts as written until it can be read back.
fn verify(path: &Path, edit: &TagEdit) -> bool {
    let Ok(values) = read_one(path) else {
        return false;
    };
    edit.fields.iter().all(|change| {
        let wanted = change.value.as_deref().map(str::trim).filter(|v| !v.is_empty());
        values.get(&change.field).map(String::as_str) == wanted
    })
}

/// A new tag of `tag_type`, carrying whatever the file already said.
fn seed(tag_type: TagType, existing: Option<&Tag>) -> Tag {
    let mut tag = Tag::new(tag_type);
    let Some(existing) = existing else {
        return tag;
    };
    for field in FIELDS {
        if let Some(value) = existing.get_string(field.key()) {
            tag.insert_text(field.key(), value.to_owned());
        }
    }
    for picture in existing.pictures() {
        tag.push_picture(picture.clone());
    }
    tag
}

fn apply(tag: &mut Tag, edit: &TagEdit, cover: Option<&Cover>) -> Result<()> {
    for change in &edit.fields {
        let key = change.field.key();
        match change.value.as_deref().map(str::trim) {
            Some(value) if !value.is_empty() => {
                if change.field.is_numeric() && value.parse::<u32>().is_err() {
                    anyhow::bail!("{:?} must be a number, got {value:?}", change.field);
                }
                tag.insert_text(key, value.to_owned());
            }
            // Null or empty clears the field. Both mean the same thing to a
            // person looking at an empty box.
            _ => {
                tag.remove_key(key);
            }
        }
    }

    match cover {
        Some(Cover::Remove) => {
            while !tag.pictures().is_empty() {
                tag.remove_picture(0);
            }
        }
        Some(Cover::Set(bytes)) => {
            let picture = Picture::from_reader(&mut std::io::Cursor::new(bytes))
                .context("reading the supplied image")?;
            anyhow::ensure!(
                !matches!(picture.mime_type(), None | Some(MimeType::Unknown(_))),
                "unrecognised image format"
            );
            while !tag.pictures().is_empty() {
                tag.remove_picture(0);
            }
            let mut picture = picture;
            picture.set_pic_type(PictureType::CoverFront);
            tag.push_picture(picture);
        }
        None => {}
    }
    Ok(())
}

/// The one tag this file's edits go into.
///
/// Whichever convention the file already uses, and ID3v2 for a file that has
/// none. WAV and AIFF each support two -- a native text chunk and an embedded
/// ID3v2 chunk -- and writing both is not achievable here: lofty 0.25 cannot
/// keep two chunks in step across repeated writes, through either its generic
/// or its container-specific API, so editing a file twice leaves one of them
/// holding the previous edit. A file whose two chunks disagree is worse than a
/// file with one, because which artist you see then depends on which program
/// opened it.
///
/// Updating what is already there means an edit never introduces a second,
/// competing chunk, and never has to remove one either. For the case this
/// feature exists for -- a download with no tags at all -- that resolves to
/// ID3v2, which is what Rekordbox, Serato and Traktor read.
fn write_target(tagged: &lofty::file::TaggedFile) -> TagType {
    if let Some(tag) = tagged.primary_tag() {
        return tag.tag_type();
    }
    if let Some(tag) = tagged.first_tag() {
        return tag.tag_type();
    }
    tagged.primary_tag_type()
}

/// A temporary name beside the original, so the rename that follows stays
/// within one filesystem and is therefore atomic.
fn temp_beside(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    // The extension is kept. Tag writers decide what a file is partly from its
    // name, and handing one a ".dubplate-tmp" makes it guess -- which is how a
    // write can report success and leave the file unchanged.
    match path.extension() {
        Some(extension) => Ok(parent.join(format!(
            "{stem}.dubplate-tmp.{}",
            extension.to_string_lossy()
        ))),
        None => Ok(parent.join(format!("{stem}.dubplate-tmp"))),
    }
}

/// Bring the index back in step with files we have just rewritten.
///
/// This is what stops the filesystem watcher from treating our own edit as the
/// audio having been replaced. Writing the new `(mtime, size)` here means the
/// next scan skips these files entirely, so the analysis they already have --
/// tempo, key, effective bit depth, spectral cutoff -- survives an edit to a
/// title. The waveform moves with its key rather than being regenerated.
fn resync(library: &mut Library, updated: &[(i64, String)]) -> Result<()> {
    if updated.is_empty() {
        return Ok(());
    }
    let tx = library.connection_mut().transaction()?;
    for (id, path) in updated {
        let file = Path::new(path);
        let Ok(metadata) = std::fs::metadata(file) else {
            continue;
        };
        let size = metadata.len();
        let mtime = scan::mtime_of(&metadata);
        let Ok(content_key) = scan::content_key(file, size) else {
            continue;
        };
        tx.execute(
            "UPDATE tracks SET mtime = ?2, size = ?3, content_key = ?4 WHERE id = ?1",
            params![id, mtime, size as i64, content_key],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Forget the cached cover for the albums these tracks belong to.
///
/// `build_cache` only looks at albums whose `art_hash` is still null, so
/// changing an embedded cover has to clear the mark or the old image stays on
/// screen until the index is rebuilt.
pub fn invalidate_album_art(library: &Library, ids: &[i64]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "UPDATE albums SET art_hash = NULL WHERE id IN
         (SELECT DISTINCT album_id FROM tracks WHERE id IN ({placeholders}) AND album_id IS NOT NULL)"
    );
    let params = rusqlite::params_from_iter(ids.iter());
    Ok(library.connection().execute(&sql, params)?)
}

/// The old and new content keys for a set of tracks, so a caller can move
/// cached artefacts that are keyed by content instead of orphaning them.
pub fn content_keys(library: &Library, ids: &[i64]) -> Result<Vec<(i64, String)>> {
    let mut out = Vec::new();
    for id in ids {
        let key: Option<String> = library
            .connection()
            .query_row(
                "SELECT content_key FROM tracks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok();
        if let Some(key) = key {
            out.push((*id, key));
        }
    }
    Ok(out)
}

fn paths_for(library: &Library, ids: &[i64]) -> Result<Vec<(i64, String)>> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(path) = library.connection().query_row(
            "SELECT path FROM tracks WHERE id = ?1",
            params![id],
            |row| row.get::<_, String>(0),
        ) {
            out.push((*id, path));
        }
    }
    Ok(out)
}

/// Read a file's tags, retrying in relaxed mode for the same reason the scanner
/// does: real files in a real collection are not always strictly correct, and
/// refusing to edit one because a previous tagger wrote a slightly wrong header
/// helps nobody.
fn probe(path: &Path) -> Result<lofty::file::TaggedFile> {
    let attempt = |mode: ParsingMode| -> Result<lofty::file::TaggedFile, String> {
        Probe::open(path)
            .map_err(|e| e.to_string())?
            // Sniff magic bytes rather than trusting the extension, exactly as
            // the scanner does. Editing a mislabelled file has to reach the
            // same conclusion the index did, or the two disagree about what it
            // even is.
            .guess_file_type()
            .map_err(|e| e.to_string())?
            .options(ParseOptions::new().parsing_mode(mode))
            .read()
            .map_err(|e| e.to_string())
    };
    attempt(ParsingMode::BestAttempt)
        .or_else(|_| attempt(ParsingMode::Relaxed))
        .map_err(|message| anyhow::anyhow!("{message}"))
        .with_context(|| format!("reading tags from {}", path.display()))
}

/// One row of the filename-to-tags preview.
///
/// Both what the file says now and what the name suggests, because the whole
/// point of a preview is comparing them before anything is written.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamePreview {
    pub id: i64,
    pub file_name: String,
    pub current_title: Option<String>,
    pub current_artist: Option<String>,
    pub guess: crate::filename::Guess,
    /// True when the guess would actually change something. A row that agrees
    /// with the file is shown but not selected.
    pub changes: bool,
}

/// What a filename-derived write is allowed to touch.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NameFields {
    pub title: bool,
    pub artist: bool,
    pub track: bool,
    /// Overwrite a field that already has a value. Off by default, because the
    /// common case is filling in what is missing, not replacing what is there.
    pub overwrite: bool,
}

impl Default for NameFields {
    fn default() -> Self {
        Self {
            title: true,
            artist: true,
            track: true,
            overwrite: false,
        }
    }
}

/// What the filenames of these tracks suggest.
///
/// `only_missing` restricts it to tracks with no artist tag, which is the
/// population this feature exists for.
pub fn preview_names(
    library: &Library,
    ids: Option<&[i64]>,
    only_missing: bool,
) -> Result<Vec<NamePreview>> {
    let rows: Vec<(i64, String)> = match ids {
        Some(ids) => paths_for(library, ids)?,
        None => {
            let conn = library.connection();
            let sql = if only_missing {
                "SELECT id, path FROM tracks WHERE artist_id IS NULL ORDER BY path"
            } else {
                "SELECT id, path FROM tracks ORDER BY path"
            };
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        }
    };

    let mut out = Vec::with_capacity(rows.len());
    for (id, path) in rows {
        let file = Path::new(&path);
        let file_name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let guess = crate::filename::parse(&file_name);
        let current = read_one(file).unwrap_or_default();
        let current_title = current.get(&Field::Title).cloned();
        let current_artist = current.get(&Field::Artist).cloned();

        let changes = (guess.title.is_some() && guess.title != current_title)
            || (guess.artist.is_some() && guess.artist != current_artist);

        out.push(NamePreview {
            id,
            file_name,
            current_title,
            current_artist,
            guess,
            changes,
        });
    }
    Ok(out)
}

/// Write filename-derived tags to the given tracks.
///
/// The guess is recomputed here rather than sent back from the preview: the
/// parser is deterministic, so the two agree, and a client cannot ask for a
/// value the parser would not have produced.
pub fn apply_names(
    library: &mut Library,
    store: &UndoStore,
    ids: &[i64],
    fields: NameFields,
) -> Result<WriteReport> {
    let mut report = WriteReport::default();
    let paths = paths_for(library, ids)?;
    let mut updates = Vec::new();
    let mut previous = Vec::new();

    for (id, path) in paths {
        let file = Path::new(&path);
        let file_name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let guess = crate::filename::parse(&file_name);
        let existing = read_one(file).unwrap_or_default();

        let mut changes = Vec::new();
        let mut want = |field: Field, value: Option<String>, enabled: bool| {
            if !enabled {
                return;
            }
            let Some(value) = value else { return };
            // Never clear a field from a filename, and never overwrite what is
            // already there unless asked: a tag someone typed beats a guess.
            if !fields.overwrite && existing.contains_key(&field) {
                return;
            }
            if existing.get(&field) == Some(&value) {
                return;
            }
            changes.push(FieldChange {
                field,
                value: Some(value),
            });
        };
        want(Field::Title, guess.title.clone(), fields.title);
        want(Field::Artist, guess.artist.clone(), fields.artist);
        want(
            Field::TrackNumber,
            guess.track.map(|n| n.to_string()),
            fields.track,
        );

        if changes.is_empty() {
            continue;
        }
        let edit = TagEdit {
            fields: changes,
            artwork: None,
        };
        let touched: Vec<Field> = edit.fields.iter().map(|change| change.field).collect();
        let before = capture(file, &touched, false);
        match write_one(file, &edit, None) {
            Ok(()) => {
                report.written += 1;
                updates.push((id, path.clone()));
                previous.push(Previous {
                    track_id: id,
                    path: path.clone(),
                    fields: before.0,
                    artwork: before.1,
                });
                report.outcomes.push(WriteOutcome { id, path, error: None });
            }
            Err(error) => {
                report.failed += 1;
                report.outcomes.push(WriteOutcome {
                    id,
                    path,
                    error: Some(format!("{error:#}")),
                });
            }
        }
    }

    resync(library, &updates)?;
    undo::record(library, store, "Tags from filenames", &previous)?;
    Ok(report)
}
