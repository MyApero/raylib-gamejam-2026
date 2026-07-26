use anyhow::{Context, Result, anyhow, bail};
use shared::constants::{START_SAT, START_VAL};
use spacetimedb_commitlog::Decoder;
use spacetimedb_commitlog::payload::txdata::{self, Visitor};
use spacetimedb_datastore::system_tables::*;
use spacetimedb_paths::FromPathUnchecked;
use spacetimedb_paths::server::CommitLogDir;
use spacetimedb_primitives::TableId;
use spacetimedb_sats::buffer::BufReader;
use spacetimedb_sats::{AlgebraicType, AlgebraicValue, ProductType, ProductValue, bsatn};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufWriter, Read, Seek, Write};
use std::path::PathBuf;
use thiserror::Error;

const ST_TABLE_ID: TableId = TableId(1);
const ST_COLUMN_ID: TableId = TableId(2);
// Version 2 retains the island id and island placement mutations. Version 1
// saved only a cell's island-local q/r, which is not enough to reconstruct
// its world position once more than one island exists. Version 3 resolves
// each island-insert event's border color (owner's `border_color` pin, else
// their seed hue at that point in the log) into the previously-unused
// `color` field of kind-5 records — replay used to fall back to a LIVE
// lookup for this, which came up empty (flat gray border) for any island
// since deleted (`admin_delete_island`/`delete_account` remove the owner's
// `Inventory` rows too, so nothing live is left to resolve from). Must stay
// in sync with `client/src/bin/web.rs`'s `RECOVERED_HISTORY_MAGIC`.
const HISTORY_MAGIC: &[u8] = b"HEXELHIST\x04";
/// Version 4 drops the transaction offset the client never read. It cost
/// eight of the twenty-five bytes, and the working file keeps it, so nothing
/// is lost for auditing — only the download shrinks, which is what decides
/// how much history can ship at all.
const HISTORY_RECORD_BYTES: usize = 17;
/// Working file the extractor appends to, in commitlog order, one record per
/// event with the transaction offset and the sort timestamp. Never shipped:
/// the client reads the chronologically sorted `HISTORY_MAGIC` file emitted
/// from this one. Kept separately because a checkpoint resume can only append
/// to log order, while the shipped order has to be rebuilt whole every run.
const RAW_MAGIC: &[u8] = b"HEXELRAW\x01";
const RAW_RECORD_BYTES: usize = 33;
/// Start of the `island_id, q, r, color` block — identical in both formats,
/// so the shipped record is this block with the kind byte in front.
const RAW_PAYLOAD_AT: usize = 9;
const RAW_TIME_AT: usize = 25;
// Mirrors `client/src/bin/web.rs`'s `UNKNOWN_BORDER_COLOR` — a kind-5
// record's `color` when neither an explicit `border_color` pin nor the
// owner's seed hue could be resolved at extraction time (never a valid
// packed HSV value, so it's an unambiguous sentinel).
const UNKNOWN_BORDER_COLOR: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct HistoryEvent {
    kind: u8,
    island_id: u32,
    q: i32,
    r: i32,
    color: u32,
    /// When this event happened, in microseconds since the epoch — the row's
    /// own `painted_at`/`created_at`, adjusted by `record_replay_event`. The
    /// shipped file is sorted on it, which is the only thing that puts a
    /// bulk-imported world back into the order it was actually painted in.
    time_micros: i64,
}

/// A read-only commitlog repository. The production `Fs` repository requires
/// write access even for scans, so it cannot safely open a live local database.
#[derive(Clone)]
struct ReadOnlyRepo(PathBuf);

struct ReadOnlySegment {
    inner: spacetimedb_fs_utils::compression::CompressReader,
    len: u64,
}

impl Read for ReadOnlySegment {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl BufRead for ReadOnlySegment {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }
}

impl Seek for ReadOnlySegment {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

impl spacetimedb_commitlog::repo::SegmentLen for ReadOnlySegment {
    fn segment_len(&mut self) -> io::Result<u64> {
        if self.inner.is_compressed() {
            Ok(self.len)
        } else {
            let previous = self.stream_position()?;
            let length = self.seek(io::SeekFrom::End(0))?;
            if previous != length {
                self.seek(io::SeekFrom::Start(previous))?;
            }
            Ok(length)
        }
    }
}

impl spacetimedb_commitlog::repo::SegmentReader for ReadOnlySegment {
    fn sealed(&self) -> bool {
        self.inner.is_compressed()
    }
}

impl fmt::Display for ReadOnlyRepo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

impl ReadOnlyRepo {
    fn segment_path(&self, offset: u64) -> PathBuf {
        self.0.join(format!("{offset:020}.stdb.log"))
    }
}

impl spacetimedb_commitlog::repo::Repo for ReadOnlyRepo {
    type SegmentWriter = File;
    type SegmentReader = ReadOnlySegment;

    fn create_segment(
        &self,
        _offset: u64,
        _header: spacetimedb_commitlog::segment::Header,
    ) -> io::Result<Self::SegmentWriter> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "read-only commitlog extractor",
        ))
    }

    fn open_segment_reader(&self, offset: u64) -> io::Result<Self::SegmentReader> {
        let file = File::open(self.segment_path(offset))?;
        let len = file.metadata()?.len();
        let inner = spacetimedb_fs_utils::compression::CompressReader::new(file)?;
        Ok(ReadOnlySegment { inner, len })
    }

    fn open_segment_writer(&self, _offset: u64) -> io::Result<Self::SegmentWriter> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "read-only commitlog extractor",
        ))
    }

    fn remove_segment(&self, _offset: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "read-only commitlog extractor",
        ))
    }

    fn compress_segment_with(
        &self,
        _offset: u64,
        _compressor: impl spacetimedb_commitlog::repo::CompressOnce,
    ) -> io::Result<spacetimedb_commitlog::CompressionStats> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "read-only commitlog extractor",
        ))
    }

    fn existing_offsets(&self) -> io::Result<Vec<u64>> {
        let mut offsets = Vec::new();
        for entry in std::fs::read_dir(&self.0)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(offset) = name
                .strip_suffix(".stdb.log")
                .and_then(|value| value.parse().ok())
            else {
                continue;
            };
            offsets.push(offset);
        }
        offsets.sort_unstable();
        Ok(offsets)
    }

    fn get_offset_index(
        &self,
        offset: u64,
    ) -> io::Result<spacetimedb_commitlog::repo::TxOffsetIndex> {
        // This maps the existing index for read access only; it never updates
        // it. Using the index keeps every resumed pass from re-reading the
        // already-checkpointed prefix of the current segment.
        let root = CommitLogDir::from_path_unchecked(self.0.clone());
        spacetimedb_commitlog::repo::TxOffsetIndex::open_index_file(&root.index(offset))
    }
}

#[derive(Clone)]
struct Column {
    name: String,
    ty: AlgebraicType,
}

#[derive(Default)]
struct State {
    current_offset: u64,
    table_names: BTreeMap<TableId, String>,
    columns: BTreeMap<TableId, BTreeMap<usize, Column>>,
    transaction_count: u64,
    tile_inserts: u64,
    tile_deletes: u64,
    checkpoint: Option<PathBuf>,
    output: Option<BufWriter<File>>,
    output_bytes: u64,
    pending_inserts: Vec<HistoryEvent>,
    pending_deletes: Vec<HistoryEvent>,
    /// Each owner's current SEED hue (`inventory` row with
    /// `obtained_with.is_none()`), kept live as the log is walked — mirrors
    /// the client's own `seed_hues` lookup, needed so an `island` insert
    /// event can resolve its border's seed-hue fallback as of THAT point in
    /// history. Keyed by the seed row's OWN `id`, not just its hue: the
    /// commitlog visits every INSERT in a transaction before any DELETE
    /// (see `flush_replay_events`'s comment), so `reset_account` — which
    /// deletes the old seed row and inserts a new one in the SAME
    /// transaction — would otherwise have the delete (of the OLD row) fire
    /// after the insert (of the NEW row) and wipe the entry that insert just
    /// set. Storing the id lets the delete handler recognize "this is the
    /// row I'm currently tracking" and leave a fresher same-transaction
    /// insert alone. NOT persisted across a checkpoint resume: a resumed run
    /// starts this map empty, so any `island` insert processed before the
    /// map has re-observed that owner's seed row resolves to
    /// `UNKNOWN_BORDER_COLOR` instead. Only matters when resuming a partial
    /// run; a from-scratch extraction always sees every `inventory` insert
    /// in order.
    owner_seed_hue: BTreeMap<AlgebraicValue, (u64, u16)>,
    /// Latest `painted_at` seen so far — a running "now" for the point the
    /// walk has reached. Events whose own row timestamp does not say when
    /// they happened (any delete; an `island` re-insert at a new leaderboard
    /// slot, which keeps its original `created_at`) are stamped with this
    /// instead. Only cell timestamps advance it: `created_at` values arrive
    /// out of order during a bulk import and would ratchet it to the newest
    /// island before the older ones had been read.
    log_time: i64,
}

/// What a delete and an insert have to agree on to be one update. An island's
/// slot is deliberately excluded — a re-rank changes exactly that, and the
/// pair still describes one island moving.
fn dedupe_key(event: &HistoryEvent) -> (u8, u32, i32, i32) {
    match event.kind {
        0 | 1 => (0, event.island_id, event.q, event.r),
        2 | 3 => (1, 0, event.q, event.r),
        _ => (2, event.island_id, 0, 0),
    }
}

fn pack_hsv(h: u16, s: u8, v: u8) -> u32 {
    ((h as u32) << 16) | ((s as u32) << 8) | (v as u32)
}

/// `None` always encodes as an empty product (SATS' unit type), regardless
/// of which sum tag SpacetimeDB happens to assign "some" vs "none" — this
/// checks the payload shape instead of hardcoding a tag index.
fn option_is_none(value: &AlgebraicValue) -> Result<bool> {
    match value {
        AlgebraicValue::Sum(sum) => Ok(matches!(
            sum.value.as_ref(),
            AlgebraicValue::Product(product) if product.elements.is_empty()
        )),
        other => bail!("expected an Option field, found {other:?}"),
    }
}

/// Microseconds out of a `Timestamp` column. SATS may hand the special type
/// back either already flattened to its payload or still wrapped in the
/// one-field product it is defined as, so both shapes are accepted.
fn timestamp_micros(value: &AlgebraicValue) -> Result<i64> {
    match value {
        AlgebraicValue::I64(micros) => Ok(*micros),
        AlgebraicValue::Product(product) => match product.elements.as_ref() {
            [AlgebraicValue::I64(micros)] => Ok(*micros),
            _ => bail!("expected a Timestamp field, found {value:?}"),
        },
        other => bail!("expected a Timestamp field, found {other:?}"),
    }
}

fn option_u32(value: &AlgebraicValue) -> Result<Option<u32>> {
    match value {
        AlgebraicValue::Sum(sum) => match sum.value.as_ref() {
            AlgebraicValue::Product(product) if product.elements.is_empty() => Ok(None),
            AlgebraicValue::U32(inner) => Ok(Some(*inner)),
            other => bail!("expected an Option<u32> field, found {other:?}"),
        },
        other => bail!("expected an Option field, found {other:?}"),
    }
}

impl State {
    fn save_checkpoint(&mut self, path: &PathBuf) -> Result<()> {
        if let Some(output) = &mut self.output {
            output.flush()?;
        }
        let mut output = format!(
            "H\t{}\t{}\t{}\t{}\t{}\t{}\n",
            self.current_offset,
            self.transaction_count,
            self.tile_inserts,
            self.tile_deletes,
            self.output_bytes,
            self.log_time
        );
        for (table_id, name) in &self.table_names {
            output.push_str(&format!("T\t{}\t{name}\n", table_id.0));
        }
        for (table_id, columns) in &self.columns {
            for (position, column) in columns {
                let encoded = bsatn::to_vec(&column.ty)?;
                let hex = encoded
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                output.push_str(&format!(
                    "C\t{}\t{position}\t{}\t{hex}\n",
                    table_id.0, column.name
                ));
            }
        }
        std::fs::write(path, output)
            .with_context(|| format!("could not write checkpoint {}", path.display()))
    }

    fn load_checkpoint(path: &PathBuf) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("could not read checkpoint {}", path.display()))?;
        let mut state = Self::default();
        for line in contents.lines() {
            let fields = line.split('\t').collect::<Vec<_>>();
            match fields.as_slice() {
                ["H", offset, transactions, inserts, deletes] => {
                    state.current_offset = offset.parse()?;
                    state.transaction_count = transactions.parse()?;
                    state.tile_inserts = inserts.parse()?;
                    state.tile_deletes = deletes.parse()?;
                }
                ["H", offset, transactions, inserts, deletes, output_bytes] => {
                    state.current_offset = offset.parse()?;
                    state.transaction_count = transactions.parse()?;
                    state.tile_inserts = inserts.parse()?;
                    state.tile_deletes = deletes.parse()?;
                    state.output_bytes = output_bytes.parse()?;
                }
                ["H", offset, transactions, inserts, deletes, output_bytes, log_time] => {
                    state.current_offset = offset.parse()?;
                    state.transaction_count = transactions.parse()?;
                    state.tile_inserts = inserts.parse()?;
                    state.tile_deletes = deletes.parse()?;
                    state.output_bytes = output_bytes.parse()?;
                    state.log_time = log_time.parse()?;
                }
                ["T", table_id, name] => {
                    state
                        .table_names
                        .insert(TableId(table_id.parse()?), (*name).to_owned());
                }
                ["C", table_id, position, name, hex] => {
                    if hex.len() % 2 != 0 {
                        bail!("malformed type encoding in checkpoint {}", path.display());
                    }
                    let bytes = (0..hex.len())
                        .step_by(2)
                        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    state
                        .columns
                        .entry(TableId(table_id.parse()?))
                        .or_default()
                        .insert(
                            position.parse()?,
                            Column {
                                name: (*name).to_owned(),
                                ty: bsatn::from_slice(&bytes)?,
                            },
                        );
                }
                _ => bail!("malformed checkpoint line in {}", path.display()),
            }
        }
        Ok(state)
    }

    fn decode_known_system_row<'a, R: BufReader<'a>>(
        &self,
        table_id: TableId,
        reader: &mut R,
    ) -> Result<bool> {
        macro_rules! decode {
            ($row:ty) => {{
                let _: $row = bsatn::from_reader(reader)?;
                return Ok(true);
            }};
        }
        match table_id {
            ST_SEQUENCE_ID => decode!(StSequenceRow),
            ST_INDEX_ID => decode!(StIndexRow),
            ST_CONSTRAINT_ID => decode!(StConstraintRow),
            ST_MODULE_ID => decode!(StModuleRow),
            ST_CLIENT_ID => decode!(StClientRow),
            ST_VAR_ID => decode!(StVarRow),
            ST_SCHEDULED_ID => decode!(StScheduledRow),
            ST_ROW_LEVEL_SECURITY_ID => decode!(StRowLevelSecurityRow),
            ST_CONNECTION_CREDENTIALS_ID => decode!(StConnectionCredentialsRow),
            ST_VIEW_ID => decode!(StViewRow),
            ST_VIEW_PARAM_ID => decode!(StViewParamRow),
            ST_VIEW_COLUMN_ID => decode!(StViewColumnRow),
            ST_VIEW_SUB_ID => decode!(StViewSubRow),
            ST_VIEW_ARG_ID => decode!(StViewArgRow),
            ST_EVENT_TABLE_ID => decode!(StEventTableRow),
            ST_TABLE_ACCESSOR_ID => decode!(StTableAccessorRow),
            ST_INDEX_ACCESSOR_ID => decode!(StIndexAccessorRow),
            ST_COLUMN_ACCESSOR_ID => decode!(StColumnAccessorRow),
            _ => Ok(false),
        }
    }

    fn row_type(&self, table_id: TableId) -> Result<ProductType> {
        let columns = self.columns.get(&table_id).ok_or_else(|| {
            anyhow!(
                "no schema known for table {table_id:?} at transaction {}",
                self.current_offset
            )
        })?;
        let mut elements = Vec::with_capacity(columns.len());
        for (expected, column) in columns {
            if *expected != elements.len() {
                bail!(
                    "non-contiguous schema for table {table_id:?} at transaction {}: expected column {}, found {}",
                    self.current_offset,
                    elements.len(),
                    expected
                );
            }
            elements.push(column.ty.clone().into());
        }
        Ok(ProductType::new(elements.into_boxed_slice()))
    }

    fn decode_dynamic<'a, R: BufReader<'a>>(
        &self,
        table_id: TableId,
        reader: &mut R,
    ) -> Result<ProductValue> {
        let row_type = self.row_type(table_id)?;
        ProductValue::decode(&row_type, reader).map_err(Into::into)
    }

    fn table_is_replayable(&self, table_id: TableId) -> bool {
        matches!(
            self.table_names.get(&table_id).map(String::as_str),
            Some("island") | Some("island_cell") | Some("margin_cell")
        )
    }

    fn replay_event(
        &self,
        table_id: TableId,
        row: &ProductValue,
        operation: &str,
    ) -> Result<Option<HistoryEvent>> {
        if !self.table_is_replayable(table_id) {
            return Ok(None);
        }
        let columns = self.columns.get(&table_id).expect("schema was decoded");
        let field = |name| {
            columns
                .iter()
                .find_map(|(index, column)| (column.name == name).then(|| row.elements.get(*index)))
                .flatten()
        };
        let u32_field = |name| match field(name) {
            Some(spacetimedb_sats::AlgebraicValue::U32(value)) => Ok(*value),
            _ => bail!(
                "replay table {table_id:?} has invalid {name} at transaction {}",
                self.current_offset
            ),
        };
        let i32_field = |name| match field(name) {
            Some(spacetimedb_sats::AlgebraicValue::I32(value)) => Ok(*value),
            _ => bail!(
                "replay table {table_id:?} has invalid {name} at transaction {}",
                self.current_offset
            ),
        };
        let time_field = |name| match field(name) {
            Some(value) => timestamp_micros(value),
            None => bail!(
                "replay table {table_id:?} is missing {name} at transaction {}",
                self.current_offset
            ),
        };
        let is_insert = match operation {
            "insert" => true,
            "delete" => false,
            _ => unreachable!(),
        };
        let table_name = self.table_names.get(&table_id).map(String::as_str);
        let event = match table_name {
            Some("island_cell") => HistoryEvent {
                kind: if is_insert { 1 } else { 0 },
                island_id: u32_field("island_id")?,
                q: i32_field("q")?,
                r: i32_field("r")?,
                color: u32_field("color")?,
                time_micros: time_field("painted_at")?,
            },
            Some("margin_cell") => HistoryEvent {
                kind: if is_insert { 3 } else { 2 },
                island_id: 0,
                q: i32_field("q")?,
                r: i32_field("r")?,
                color: u32_field("color")?,
                time_micros: time_field("painted_at")?,
            },
            Some("island") => {
                // Resolved once here rather than left for replay to look up
                // live: an island's owner and `border_color` pin at THIS
                // point in history may no longer exist in the live database
                // (deleted since), so the border color has to travel with
                // the event itself. Only meaningful for inserts — a delete
                // event's `color` is unused, same as before.
                let color = if is_insert {
                    let border_color = match field("border_color") {
                        Some(value) => option_u32(value)?,
                        None => bail!(
                            "island row is missing border_color at transaction {}",
                            self.current_offset
                        ),
                    };
                    border_color.unwrap_or_else(|| {
                        field("owner")
                            .and_then(|owner| self.owner_seed_hue.get(owner))
                            .map(|&(_, hue)| pack_hsv(hue, START_SAT, START_VAL))
                            .unwrap_or(UNKNOWN_BORDER_COLOR)
                    })
                } else {
                    0
                };
                HistoryEvent {
                    kind: if is_insert { 5 } else { 4 },
                    island_id: u32_field("id")?,
                    q: u32_field("slot")? as i32,
                    r: 0,
                    color,
                    time_micros: time_field("created_at")?,
                }
            }
            _ => unreachable!("validated by table_is_replayable"),
        };
        Ok(Some(event))
    }

    /// Keeps `owner_seed_hue` current as the log is walked — called on every
    /// row, a no-op for anything but `inventory`'s SEED row
    /// (`obtained_with.is_none()`; exactly one exists per owner at a time,
    /// replaced wholesale by `reset_account`'s reseed). Must run before
    /// `record_replay_event` sees the SAME row so an `island` insert in the
    /// same transaction as a brand-new owner's seed row (`client_connected`
    /// inserts `inventory` before `island`) can already resolve it.
    fn track_inventory_seed(
        &mut self,
        table_id: TableId,
        row: &ProductValue,
        operation: &str,
    ) -> Result<()> {
        if self.table_names.get(&table_id).map(String::as_str) != Some("inventory") {
            return Ok(());
        }
        let columns = self.columns.get(&table_id).expect("schema was decoded");
        let field = |name| {
            columns
                .iter()
                .find_map(|(index, column)| (column.name == name).then(|| row.elements.get(*index)))
                .flatten()
        };
        let is_seed = match field("obtained_with") {
            Some(value) => option_is_none(value)?,
            None => bail!(
                "inventory row is missing obtained_with at transaction {}",
                self.current_offset
            ),
        };
        if !is_seed {
            return Ok(());
        }
        let Some(owner) = field("owner").cloned() else {
            bail!(
                "inventory row is missing owner at transaction {}",
                self.current_offset
            );
        };
        let id = match field("id") {
            Some(AlgebraicValue::U64(value)) => *value,
            _ => bail!(
                "inventory.id has an unexpected shape at transaction {}",
                self.current_offset
            ),
        };
        let hue = match field("hue") {
            Some(AlgebraicValue::U16(value)) => *value,
            _ => bail!(
                "inventory.hue has an unexpected shape at transaction {}",
                self.current_offset
            ),
        };
        match operation {
            "insert" => {
                self.owner_seed_hue.insert(owner, (id, hue));
            }
            "delete" => {
                // Only clear if this delete is for the row currently
                // tracked — a same-transaction insert of a REPLACEMENT seed
                // row (already applied above, since inserts are visited
                // first) must survive the old row's trailing delete.
                if self.owner_seed_hue.get(&owner).is_some_and(|&(current_id, _)| current_id == id) {
                    self.owner_seed_hue.remove(&owner);
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn record_replay_event(
        &mut self,
        table_id: TableId,
        row: &ProductValue,
        operation: &str,
    ) -> Result<()> {
        let Some(event) = self.replay_event(table_id, row, operation)? else {
            return Ok(());
        };
        // A cell insert's `painted_at` is when it happened, so it is the only
        // event that both times itself and advances the running clock. The
        // rest are stamped at the end of the transaction — see
        // `flush_replay_events`.
        if matches!(event.kind, 1 | 3) {
            self.log_time = self.log_time.max(event.time_micros);
        }
        match operation {
            "insert" => {
                self.tile_inserts += 1;
                self.pending_inserts.push(event);
            }
            "delete" => {
                self.tile_deletes += 1;
                self.pending_deletes.push(event);
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn write_event(&mut self, event: HistoryEvent) -> Result<()> {
        let Some(output) = &mut self.output else {
            return Ok(());
        };
        output.write_all(&[event.kind])?;
        output.write_all(&self.current_offset.to_le_bytes())?;
        output.write_all(&event.island_id.to_le_bytes())?;
        output.write_all(&event.q.to_le_bytes())?;
        output.write_all(&event.r.to_le_bytes())?;
        output.write_all(&event.color.to_le_bytes())?;
        output.write_all(&event.time_micros.to_le_bytes())?;
        self.output_bytes += RAW_RECORD_BYTES as u64;
        Ok(())
    }

    fn flush_replay_events(&mut self) -> Result<()> {
        // Commitlog stores inserts before deletes. A paint overwrite is atomic,
        // though, so apply deletes first to leave the newly inserted color
        // visible at the transaction boundary.
        let deletes = std::mem::take(&mut self.pending_deletes);
        let inserts = std::mem::take(&mut self.pending_inserts);
        // An update reaches the log as a delete plus an insert of the same
        // row. Replay applies an insert by overwriting, so writing the delete
        // too only makes the thing blink out and back: a whole leaderboard
        // re-rank vanished and reappeared, and every repainted tile flashed
        // empty. Only deletes with no matching insert are real removals.
        let replaced: BTreeSet<_> = inserts.iter().map(dedupe_key).collect();
        let deletes = deletes
            .into_iter()
            .filter(|event| !replaced.contains(&dedupe_key(event)));
        for mut event in deletes.chain(inserts) {
            // Stamped here, not on arrival, so that everything a transaction
            // did carries one time. Stamping as rows were visited let a
            // re-rank's delete and its matching re-insert land on either side
            // of a paint in the same transaction, which sorted the insert
            // ahead of the delete and left the island deleted for good.
            event.time_micros = match event.kind {
                1 | 3 => event.time_micros,
                // Everything else happened at the transaction's clock, but
                // never before the row it acts on was itself created: a quiet
                // spell leaves the clock behind the row, and a delete stamped
                // earlier than its own insert sorts ahead of it and never
                // takes effect. Keeping `created_at` as the floor is also
                // what makes a bulk-imported island appear when it was
                // founded rather than when the import ran.
                _ => event.time_micros.max(self.log_time),
            };
            self.write_event(event)?;
        }
        Ok(())
    }
}

impl Visitor for State {
    type Error = VisitError;
    type Row = ();

    fn visit_insert<'a, R: BufReader<'a>>(
        &mut self,
        table_id: TableId,
        reader: &mut R,
    ) -> std::result::Result<(), Self::Error> {
        (|| -> Result<()> {
            if table_id == ST_TABLE_ID {
                let table: StTableRow = bsatn::from_reader(reader)?;
                self.table_names
                    .insert(table.table_id, table.table_name.to_string());
                return Ok(());
            }
            if table_id == ST_COLUMN_ID {
                let column: StColumnRow = bsatn::from_reader(reader)?;
                self.columns.entry(column.table_id).or_default().insert(
                    column.col_pos.idx(),
                    Column {
                        name: column.col_name.to_string(),
                        ty: column.col_type.0,
                    },
                );
                return Ok(());
            }
            if self.decode_known_system_row(table_id, reader)? {
                return Ok(());
            }
            let row = self.decode_dynamic(table_id, reader)?;
            self.track_inventory_seed(table_id, &row, "insert")?;
            self.record_replay_event(table_id, &row, "insert")
        })()
        .map_err(Into::into)
    }

    fn visit_delete<'a, R: BufReader<'a>>(
        &mut self,
        table_id: TableId,
        reader: &mut R,
    ) -> std::result::Result<(), Self::Error> {
        (|| -> Result<()> {
            if table_id == ST_TABLE_ID {
                let table: StTableRow = bsatn::from_reader(reader)?;
                self.table_names.remove(&table.table_id);
                return Ok(());
            }
            if table_id == ST_COLUMN_ID {
                let column: StColumnRow = bsatn::from_reader(reader)?;
                let position = column.col_pos.idx();
                if self
                    .columns
                    .get(&column.table_id)
                    .and_then(|columns| columns.get(&position))
                    .is_some_and(|current| current.name == column.col_name.to_string())
                {
                    self.columns
                        .entry(column.table_id)
                        .or_default()
                        .remove(&position);
                }
                return Ok(());
            }
            if self.decode_known_system_row(table_id, reader)? {
                return Ok(());
            }
            let row = self.decode_dynamic(table_id, reader)?;
            self.track_inventory_seed(table_id, &row, "delete")?;
            self.record_replay_event(table_id, &row, "delete")
        })()
        .map_err(Into::into)
    }

    fn skip_row<'a, R: BufReader<'a>>(
        &mut self,
        table_id: TableId,
        reader: &mut R,
    ) -> std::result::Result<(), Self::Error> {
        if self
            .decode_known_system_row(table_id, reader)
            .map_err(VisitError::from)?
        {
            return Ok(());
        }
        self.decode_dynamic(table_id, reader)
            .map(drop)
            .map_err(Into::into)
    }

    fn visit_tx_start(&mut self, offset: u64) -> std::result::Result<(), Self::Error> {
        debug_assert!(self.pending_inserts.is_empty() && self.pending_deletes.is_empty());
        self.current_offset = offset;
        self.transaction_count += 1;
        Ok(())
    }

    fn visit_tx_end(&mut self) -> std::result::Result<(), Self::Error> {
        self.flush_replay_events().map_err(VisitError::from)?;
        if self.transaction_count.is_multiple_of(250_000) {
            if let Some(path) = self.checkpoint.clone() {
                self.save_checkpoint(&path).map_err(VisitError::from)?;
            }
            eprintln!(
                "scanned_transactions={} latest_offset={}",
                self.transaction_count, self.current_offset
            );
        }
        Ok(())
    }
}

struct Extractor(RefCell<State>);

#[derive(Debug, Error)]
enum VisitError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Decode(#[from] spacetimedb_sats::buffer::DecodeError),
}

impl From<anyhow::Error> for VisitError {
    fn from(error: anyhow::Error) -> Self {
        Self::Message(error.to_string())
    }
}

#[derive(Debug, Error)]
enum ExtractError {
    #[error(transparent)]
    Decode(#[from] txdata::DecoderError<VisitError>),
    #[error(transparent)]
    RawDecode(#[from] spacetimedb_sats::buffer::DecodeError),
    #[error(transparent)]
    Traversal(#[from] spacetimedb_commitlog::error::Traversal),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Decoder for &Extractor {
    type Record = ();
    type Error = ExtractError;

    fn decode_record<'a, R: BufReader<'a>>(
        &self,
        version: u8,
        tx_offset: u64,
        reader: &mut R,
    ) -> Result<Self::Record, Self::Error> {
        txdata::decode_record_fn(&mut *self.0.borrow_mut(), version, tx_offset, reader)
            .map(|_| ())
            .map_err(Into::into)
    }

    fn consume_record<'a, R: BufReader<'a>>(
        &self,
        version: u8,
        tx_offset: u64,
        reader: &mut R,
    ) -> Result<(), Self::Error> {
        txdata::consume_record_fn(&mut *self.0.borrow_mut(), version, tx_offset, reader)
            .map_err(Into::into)
    }

    fn skip_record<'a, R: BufReader<'a>>(
        &self,
        version: u8,
        _tx_offset: u64,
        reader: &mut R,
    ) -> Result<(), Self::Error> {
        txdata::skip_record_fn(&mut *self.0.borrow_mut(), version, reader).map_err(Into::into)
    }
}

type RawRecord = [u8; RAW_RECORD_BYTES];

fn record_time(record: &RawRecord) -> i64 {
    i64::from_le_bytes(record[RAW_TIME_AT..].try_into().expect("fixed raw record"))
}

fn record_island(record: &RawRecord) -> u32 {
    u32::from_le_bytes(
        record[RAW_PAYLOAD_AT..RAW_PAYLOAD_AT + 4]
            .try_into()
            .expect("fixed raw record"),
    )
}

fn read_raw(path: &PathBuf) -> Result<Vec<RawRecord>> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("could not read history output {}", path.display()))?;
    let body = bytes
        .strip_prefix(RAW_MAGIC)
        .with_context(|| format!("{} has an unknown format", path.display()))?;
    if body.len() % RAW_RECORD_BYTES != 0 {
        bail!("{} has a partial record", path.display());
    }
    Ok(body
        .chunks_exact(RAW_RECORD_BYTES)
        .map(|chunk| chunk.try_into().expect("fixed raw record"))
        .collect())
}

/// Rewrite the log-order working file as the chronologically ordered file the
/// client ships, dropping the sort timestamp from each record.
///
/// The whole history is sorted every run rather than appended to, because a
/// bulk import lands thousands of events whose real times are spread over
/// weeks: they only fall into place relative to each other once the entire
/// set is ordered together. The sort is stable, so events sharing a timestamp
/// keep the log's own order — which is what keeps a delete ahead of the
/// insert that replaces it within one transaction.
///
/// `legacy_path` is an optional working file extracted from a commitlog this
/// database was seeded from. Only its island placement events are merged in:
/// they are what a bulk import cannot carry (every island arrives at the slot
/// it held on import day), and they are a few thousand records against the
/// millions of paint events in the same log.
fn emit_sorted(
    raw_path: &PathBuf,
    sorted_path: &PathBuf,
    legacy_path: Option<&PathBuf>,
) -> Result<usize> {
    let current = read_raw(raw_path)?;
    let imported: BTreeSet<u32> = current
        .iter()
        .filter(|record| record[0] == 5)
        .map(record_island)
        .collect();
    let mut records: Vec<RawRecord> = Vec::new();
    let mut legacy_islands = BTreeSet::new();
    let mut legacy_alive: BTreeMap<u32, RawRecord> = BTreeMap::new();
    let mut legacy_end = i64::MIN;
    if let Some(path) = legacy_path {
        for record in read_raw(path)? {
            // Over every record, not just the island ones: painting carries on
            // after the last re-rank, and an end taken from island events
            // alone put the synthesised deletes below in the middle of it,
            // orphaning every cell those islands were painted after.
            legacy_end = legacy_end.max(record_time(&record));
            if matches!(record[0], 4 | 5) {
                let island = record_island(&record);
                legacy_islands.insert(island);
                if record[0] == 5 {
                    legacy_alive.insert(island, record);
                } else {
                    legacy_alive.remove(&island);
                }
            }
            records.push(record);
        }
    }
    // The recovered history stops before the import was taken, so islands
    // reaped in between are still standing at its last event and nothing in
    // the imported world ever removes them — they would sit on top of
    // whichever island later inherits the slot, for the rest of the replay.
    // Absent from the import is the evidence they are gone; the delete is
    // synthesised from each one's own last event, so it keeps its offset.
    for (island, mut record) in legacy_alive {
        if !imported.contains(&island) {
            record[0] = 4;
            record[RAW_TIME_AT..].copy_from_slice(&legacy_end.to_le_bytes());
            records.push(record);
        }
    }
    for mut record in current {
        // The import that seeded this database gave every island the slot it
        // held on import day, timestamped with when that island was founded.
        // With the real placement history in front of it that event is no
        // longer a founding, it is the catch-up to the layout the import
        // froze — so it belongs where the recovered history runs out.
        if matches!(record[0], 4 | 5)
            && legacy_islands.contains(&record_island(&record))
            && record_time(&record) < legacy_end
        {
            record[RAW_TIME_AT..].copy_from_slice(&legacy_end.to_le_bytes());
        }
        records.push(record);
    }
    records.sort_by_key(record_time);
    let mut output = BufWriter::new(File::create(sorted_path).with_context(|| {
            format!("could not create shipped history {}", sorted_path.display())
        })?);
    output.write_all(HISTORY_MAGIC)?;
    for record in &records {
        output.write_all(&record[..1])?;
        output.write_all(&record[RAW_PAYLOAD_AT..RAW_TIME_AT])?;
    }
    output.flush()?;
    Ok(records.len())
}

/// Re-read what was just written and refuse to ship it if it cannot be
/// replayed coherently.
///
/// Every bug this file has had was silent: the replay still rendered, just
/// wrong — cells belonging to islands that did not exist yet, islands stacked
/// in one slot, a delete sorted ahead of the insert it was meant to replace.
/// None of it is visible without playing the whole thing back and looking
/// carefully, so it gets checked here instead, where a failure stops the
/// refresh rather than reaching a player.
fn validate_shipped(path: &PathBuf) -> Result<()> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("could not re-read shipped history {}", path.display()))?;
    let body = bytes
        .strip_prefix(HISTORY_MAGIC)
        .context("shipped history does not carry the expected magic")?;
    if body.len() % HISTORY_RECORD_BYTES != 0 {
        bail!("shipped history has a partial record");
    }
    let mut slots: BTreeMap<u32, i32> = BTreeMap::new();
    let mut orphans = 0_u64;
    let mut events = 0_u64;
    for record in body.chunks_exact(HISTORY_RECORD_BYTES) {
        events += 1;
        let island = u32::from_le_bytes(record[1..5].try_into().expect("fixed record"));
        let q = i32::from_le_bytes(record[5..9].try_into().expect("fixed record"));
        match record[0] {
            5 => {
                slots.insert(island, q);
            }
            4 => {
                slots.remove(&island);
            }
            // A cell painted onto an island that is not standing has nowhere
            // to be drawn, so replay skips it — history that silently never
            // appears. Its DELETE is not a problem the same way: reaping an
            // island drops the island and its cells in one transaction, so
            // those deletes legitimately land just after it is gone, and
            // removing a cell nobody is drawing is a no-op either way.
            1 if !slots.contains_key(&island) => orphans += 1,
            0..=3 => {}
            other => bail!("shipped history has an unknown event kind {other}"),
        }
    }
    if events == 0 {
        bail!("shipped history is empty");
    }
    if orphans > 0 {
        bail!("shipped history has {orphans} cell events whose island is not present at that point");
    }
    if slots.is_empty() {
        bail!("shipped history ends with no islands standing");
    }
    // One island per slot is a database invariant (`island.slot` is unique),
    // so two sharing one at the end means events were lost or misordered.
    let mut occupants: BTreeMap<i32, u32> = BTreeMap::new();
    for (&island, &slot) in &slots {
        if let Some(&other) = occupants.get(&slot) {
            bail!("shipped history ends with islands #{other} and #{island} both in slot {slot}");
        }
        occupants.insert(slot, island);
    }
    println!("validated_events={events}");
    println!("validated_islands={}", slots.len());
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .context("usage: history-extractor /path/to/replica/clog")?;
    let checkpoint = args.next().map(PathBuf::from);
    let output_path = args.next().map(PathBuf::from);
    let sorted_path = args.next().map(PathBuf::from);
    let legacy_path = args.next().map(PathBuf::from).filter(|path| path.exists());
    let mut state = match checkpoint.as_ref().filter(|path| path.exists()) {
        Some(path) => State::load_checkpoint(path)?,
        None => State::default(),
    };
    let start_offset = if state.transaction_count == 0 {
        0
    } else {
        state.current_offset.saturating_add(1)
    };
    state.checkpoint = checkpoint.clone();
    if let Some(output_path) = output_path.clone() {
        let file = if state.transaction_count == 0 {
            let mut file = File::create(&output_path).with_context(|| {
                format!("could not create history output {}", output_path.display())
            })?;
            file.write_all(RAW_MAGIC)?;
            state.output_bytes = RAW_MAGIC.len() as u64;
            file
        } else {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&output_path)
                .with_context(|| {
                    format!("could not resume history output {}", output_path.display())
                })?;
            file.set_len(state.output_bytes)?;
            file.seek(io::SeekFrom::End(0))?;
            file
        };
        state.output = Some(BufWriter::new(file));
    }
    let extractor = Extractor(RefCell::new(state));
    spacetimedb_commitlog::commitlog::fold_transactions_from(
        ReadOnlyRepo(path),
        spacetimedb_commitlog::DEFAULT_LOG_FORMAT_VERSION,
        start_offset,
        &extractor,
    )
    .context("could not decode commitlog")?;
    let mut state = extractor.0.into_inner();
    if let Some(path) = &checkpoint {
        state.save_checkpoint(path)?;
    }
    if let (Some(raw), Some(sorted)) = (output_path, sorted_path) {
        let events = emit_sorted(&raw, &sorted, legacy_path.as_ref())?;
        println!("shipped_events={events}");
        validate_shipped(&sorted)?;
    }
    println!("transactions={}", state.transaction_count);
    println!("tile_inserts={}", state.tile_inserts);
    println!("tile_deletes={}", state.tile_deletes);
    for (id, name) in state.table_names {
        if matches!(name.as_str(), "island_cell" | "margin_cell") {
            println!("tile_table={id:?}:{name}");
        }
    }
    Ok(())
}
