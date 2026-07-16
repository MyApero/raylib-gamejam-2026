use anyhow::{Context, Result, anyhow, bail};
use spacetimedb_commitlog::Decoder;
use spacetimedb_commitlog::payload::txdata::{self, Visitor};
use spacetimedb_datastore::system_tables::*;
use spacetimedb_paths::FromPathUnchecked;
use spacetimedb_paths::server::CommitLogDir;
use spacetimedb_primitives::TableId;
use spacetimedb_sats::buffer::BufReader;
use spacetimedb_sats::{AlgebraicType, ProductType, ProductValue, bsatn};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufWriter, Read, Seek, Write};
use std::path::PathBuf;
use thiserror::Error;

const ST_TABLE_ID: TableId = TableId(1);
const ST_COLUMN_ID: TableId = TableId(2);
// Version 2 retains the island id and island placement mutations. Version 1
// saved only a cell's island-local q/r, which is not enough to reconstruct
// its world position once more than one island exists.
const HISTORY_MAGIC: &[u8] = b"HEXELHIST\x02";

#[derive(Clone, Copy)]
struct HistoryEvent {
    kind: u8,
    island_id: u32,
    q: i32,
    r: i32,
    color: u32,
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
}

impl State {
    fn save_checkpoint(&mut self, path: &PathBuf) -> Result<()> {
        if let Some(output) = &mut self.output {
            output.flush()?;
        }
        let mut output = format!(
            "H\t{}\t{}\t{}\t{}\t{}\n",
            self.current_offset,
            self.transaction_count,
            self.tile_inserts,
            self.tile_deletes,
            self.output_bytes
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
            },
            Some("margin_cell") => HistoryEvent {
                kind: if is_insert { 3 } else { 2 },
                island_id: 0,
                q: i32_field("q")?,
                r: i32_field("r")?,
                color: u32_field("color")?,
            },
            Some("island") => HistoryEvent {
                kind: if is_insert { 5 } else { 4 },
                island_id: u32_field("id")?,
                q: u32_field("slot")? as i32,
                r: 0,
                color: 0,
            },
            _ => unreachable!("validated by table_is_replayable"),
        };
        Ok(Some(event))
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
        self.output_bytes += 25;
        Ok(())
    }

    fn flush_replay_events(&mut self) -> Result<()> {
        // Commitlog stores inserts before deletes. A paint overwrite is atomic,
        // though, so apply deletes first to leave the newly inserted color
        // visible at the transaction boundary.
        let deletes = std::mem::take(&mut self.pending_deletes);
        let inserts = std::mem::take(&mut self.pending_inserts);
        for event in deletes.into_iter().chain(inserts) {
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

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .context("usage: history-extractor /path/to/replica/clog")?;
    let checkpoint = args.next().map(PathBuf::from);
    let output_path = args.next().map(PathBuf::from);
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
    if let Some(output_path) = output_path {
        let file = if state.transaction_count == 0 {
            let mut file = File::create(&output_path).with_context(|| {
                format!("could not create history output {}", output_path.display())
            })?;
            file.write_all(HISTORY_MAGIC)?;
            state.output_bytes = HISTORY_MAGIC.len() as u64;
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
