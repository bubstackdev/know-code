//! Indexed, immutable pages over an already bounded common-core result.
use crate::{
    Error, Result,
    execution::{self, Execution},
    protocol::WorkspaceStatus,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Seek, SeekFrom},
};

const MAGIC: &[u8; 8] = b"KCPAGE01";
const VERSION: u32 = 1;
const FIXED: usize = 136;
const ENTRY: usize = 84;
const MAX_HEADER: usize = execution::MAX_RESPONSE_BYTES as usize + 4096;
const MAX_PREFIX: usize = 256;
const MAX_FILE: usize = super::MAX_RESULT_BYTES as usize;

pub(crate) struct EncodeOptions<'a> {
    pub op: &'a str,
    pub total_limit: u32,
    pub page_size: u32,
    pub cursor_prefix: &'a str,
    pub response_max_bytes: usize,
}
pub(crate) struct ReadOptions<'a> {
    pub position: u32,
    pub cursor_prefix: &'a str,
    pub expected_generation: u64,
    pub response_max_bytes: usize,
    pub freshness: &'a str,
    pub coordination: Option<&'a WorkspaceStatus>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    name: String,
    total: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    op: String,
    total_limit: u32,
    page_size: u32,
    page_count: u32,
    cursor_prefix: String,
    sections: Vec<Section>,
    result: Value,
}
#[derive(Clone, Copy)]
struct Plan {
    start: u32,
    count: u32,
}
struct Preamble {
    bytes: u32,
    generation: u64,
    header_len: u32,
    page_count: u32,
    page_size: u32,
    total: u32,
    header_hash: [u8; 32],
}
struct Entry {
    offset: u32,
    bytes: u32,
    plan: Plan,
    data_hash: [u8; 32],
}
#[derive(Serialize)]
struct Progress {
    total: u32,
    start: u32,
    count: u32,
}
#[derive(Serialize)]
struct Page<'a> {
    cursor: String,
    next_cursor: Option<String>,
    position: u32,
    page_count: u32,
    total_limit: u32,
    page_size: u32,
    sections: BTreeMap<&'a str, Progress>,
}

fn scope() -> Error {
    Error::InvalidRequest(
        "materialized result or pagination options do not match the query scope".into(),
    )
}
fn too_large() -> Error {
    Error::ResourceLimited("encoded saved result exceeds the 16777216-byte file guard".into())
}
fn limit(maximum: usize) -> Result<()> {
    execution::response_budget(Some(u32::try_from(maximum).map_err(|_| scope())?)).map(|_| ())
}
fn names(op: &str) -> Result<&'static [&'static str]> {
    Ok(match op {
        "symbols" => &["symbols"],
        "references" => &["occurrences"],
        "symbol_context" => &["contexts"],
        "coverage" => &["coverage"],
        "callers" | "callees" | "impact" => &["findings", "boundaries"],
        _ => return Err(scope()),
    })
}
fn graph(op: &str) -> bool {
    matches!(op, "callers" | "callees" | "impact")
}
fn arrays<'a>(result: &'a mut Value, op: &str) -> Result<&'a mut Map<String, Value>> {
    let root = result.as_object_mut().ok_or_else(scope)?;
    if graph(op) {
        root.get_mut("graph")
            .and_then(Value::as_object_mut)
            .ok_or_else(scope)
    } else {
        Ok(root)
    }
}
fn hash(bytes: &[u8], execution: Execution) -> Result<[u8; 32]> {
    let mut digest = Sha256::new();
    for chunk in bytes.chunks(16 * 1024) {
        execution.check()?;
        digest.update(chunk);
    }
    execution.check()?;
    Ok(digest.finalize().into())
}
fn index_hash(header: &[u8; 32], bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(header);
    digest.update(bytes);
    digest.finalize().into()
}
fn token(prefix: &str, position: u32) -> String {
    format!("{prefix}{position:08x}")
}
fn progress(header: &Header, plan: Plan) -> BTreeMap<&str, Progress> {
    let mut offset = 0u32;
    header
        .sections
        .iter()
        .map(|section| {
            let end = offset + section.total;
            let start = plan.start.clamp(offset, end) - offset;
            let count = (plan.start + plan.count)
                .min(end)
                .saturating_sub(plan.start.max(offset));
            offset = end;
            (
                section.name.as_str(),
                Progress {
                    total: section.total,
                    start,
                    count,
                },
            )
        })
        .collect()
}
fn page(header: &Header, position: u32, plan: Plan) -> Page<'_> {
    Page {
        cursor: token(&header.cursor_prefix, position),
        next_cursor: (position + 1 < header.page_count)
            .then(|| token(&header.cursor_prefix, position + 1)),
        position,
        page_count: header.page_count,
        total_limit: header.total_limit,
        page_size: header.page_size,
        sections: progress(header, plan),
    }
}
fn encoded(value: &impl Serialize, execution: Execution, maximum: usize) -> Result<Vec<u8>> {
    execution::write_json(value, Some(execution), maximum, Vec::new())
}
fn row_bytes(prefix: &[usize], plan: Plan) -> usize {
    prefix[(plan.start + plan.count) as usize] - prefix[plan.start as usize]
}
fn fits(
    header: &Header,
    position: u32,
    plan: Plan,
    prefix: &[usize],
    base: usize,
    maximum: usize,
    execution: Execution,
) -> Result<bool> {
    let mut metadata = page(header, position, plan);
    metadata.next_cursor = ((plan.start + plan.count) as usize + 1 < prefix.len())
        .then(|| token(&header.cursor_prefix, position + 1));
    let commas: usize = metadata
        .sections
        .values()
        .map(|section| section.count.saturating_sub(1) as usize)
        .sum();
    let metadata = encoded(&metadata, execution, MAX_HEADER)?;
    // base includes the outer result envelope and empty arrays. Add the new
    // object member comma + "page":, metadata and complete atomic row bytes.
    Ok(base + 8 + metadata.len() + row_bytes(prefix, plan) + commas <= maximum)
}
fn partitions(
    header: &Header,
    prefix: &[usize],
    total: u32,
    base: usize,
    maximum: usize,
    execution: Execution,
) -> Result<Vec<Plan>> {
    if total == 0 {
        let plan = Plan { start: 0, count: 0 };
        if !fits(header, 0, plan, prefix, base, maximum, execution)? {
            return Err(Error::ResponseLimitExceeded);
        }
        return Ok(vec![plan]);
    }
    let mut plans = Vec::new();
    let mut start = 0;
    while start < total {
        execution.check()?;
        if FIXED + (plans.len() + 1) * ENTRY + prefix[total as usize] > MAX_FILE {
            return Err(too_large());
        }
        let position = u32::try_from(plans.len()).map_err(|_| too_large())?;
        let remaining = total - start;
        let upper = header.page_size.min(remaining);
        // A terminal null token is shorter than a nonterminal token. Try the
        // whole tail first: a smaller nonterminal prefix might not fit at all.
        let tail = Plan {
            start,
            count: remaining,
        };
        if remaining <= header.page_size
            && fits(header, position, tail, prefix, base, maximum, execution)?
        {
            plans.push(tail);
            break;
        }
        let (mut low, mut high, mut best) = (1, upper, 0);
        while low <= high {
            let count = low + (high - low) / 2;
            if fits(
                header,
                position,
                Plan { start, count },
                prefix,
                base,
                maximum,
                execution,
            )? {
                best = count;
                low = count + 1;
            } else {
                high = count - 1;
            }
        }
        if best == 0 {
            return Err(Error::ResponseLimitExceeded);
        }
        plans.push(Plan { start, count: best });
        start += best;
    }
    Ok(plans)
}

pub(crate) fn encode(
    mut result: Value,
    options: EncodeOptions<'_>,
    execution: Execution,
) -> Result<Vec<u8>> {
    execution.check()?;
    limit(options.response_max_bytes)?;
    if !(1..=1000).contains(&options.total_limit)
        || !(1..=1000).contains(&options.page_size)
        || options.cursor_prefix.is_empty()
        || options.cursor_prefix.len() > MAX_PREFIX
    {
        return Err(scope());
    }
    let fields = names(options.op)?;
    let root = result.as_object().ok_or_else(scope)?;
    let generation = root
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|g| *g > 0)
        .ok_or_else(scope)?;
    if root.contains_key("page")
        || !root.get("truncated").is_some_and(Value::is_boolean)
        || !root.get("freshness").is_some_and(Value::is_string)
    {
        return Err(scope());
    }
    for other in ["symbols", "occurrences", "contexts"] {
        if fields.contains(&other) {
            continue;
        }
        if root
            .get(other)
            .is_some_and(|value| value.as_array().is_none_or(|rows| !rows.is_empty()))
        {
            return Err(scope());
        }
    }
    if !graph(options.op) && root.get("graph").is_some_and(|value| !value.is_null()) {
        return Err(scope());
    }
    let target = arrays(&mut result, options.op)?;
    let mut rows = Vec::new();
    let mut sections = Vec::new();
    for (index, name) in fields.iter().enumerate() {
        let section = match target.insert((*name).into(), Value::Array(Vec::new())) {
            Some(Value::Array(rows)) => rows,
            None => Vec::new(),
            _ => return Err(scope()),
        };
        let total = u32::try_from(section.len()).map_err(|_| too_large())?;
        if index == 0 && total > options.total_limit {
            return Err(scope());
        }
        sections.push(Section {
            name: (*name).into(),
            total,
        });
        rows.extend(section);
    }
    let total = u32::try_from(rows.len()).map_err(|_| too_large())?;
    let minimum_pages = total.max(1).div_ceil(options.page_size);
    if FIXED + minimum_pages as usize * ENTRY > MAX_FILE {
        return Err(too_large());
    }
    let mut header = Header {
        op: options.op.into(),
        total_limit: options.total_limit,
        page_size: options.page_size,
        page_count: 1,
        cursor_prefix: options.cursor_prefix.into(),
        sections,
        result,
    };
    let base = execution::encode(&header.result, execution, options.response_max_bytes)?.len();
    let mut items = Vec::with_capacity(rows.len());
    let mut prefix = vec![0usize];
    for row in rows {
        execution.check()?;
        let bytes = encoded(&row, execution, options.response_max_bytes)?;
        let next = prefix
            .last()
            .unwrap()
            .checked_add(bytes.len())
            .filter(|value| *value <= MAX_FILE)
            .ok_or_else(too_large)?;
        prefix.push(next);
        items.push(bytes);
    }
    let mut selected = None;
    // Only the decimal width of page_count affects metadata sizes. Starting
    // at one, widths can grow at most ten times; rows are never reserialized.
    for _ in 0..10 {
        let plans = partitions(
            &header,
            &prefix,
            total,
            base,
            options.response_max_bytes,
            execution,
        )?;
        let count = u32::try_from(plans.len()).map_err(|_| too_large())?;
        if header.page_count.to_string().len() == count.to_string().len() {
            header.page_count = count;
            selected = Some(plans);
            break;
        }
        header.page_count = count;
    }
    let plans = selected.ok_or_else(too_large)?;
    let bytes = encoded(&header, execution, MAX_HEADER)?;
    let header_hash = hash(&bytes, execution)?;
    let data_start = FIXED + bytes.len() + plans.len() * ENTRY;
    let payload: usize = plans
        .iter()
        .map(|plan| 2 + row_bytes(&prefix, *plan) + plan.count.saturating_sub(1) as usize)
        .sum();
    let file_bytes = data_start
        .checked_add(payload)
        .filter(|length| *length <= MAX_FILE)
        .ok_or_else(too_large)?;
    let mut output = Vec::with_capacity(file_bytes);
    output.resize(FIXED, 0);
    output.extend_from_slice(&bytes);
    output.resize(data_start, 0);
    for (position, plan) in plans.into_iter().enumerate() {
        execution.check()?;
        let offset = output.len();
        output.push(b'[');
        for (index, item) in items[plan.start as usize..(plan.start + plan.count) as usize]
            .iter()
            .enumerate()
        {
            if index > 0 {
                output.push(b',');
            }
            for chunk in item.chunks(16 * 1024) {
                execution.check()?;
                output.extend_from_slice(chunk);
            }
        }
        output.push(b']');
        let end = output.len();
        let data_hash = hash(&output[offset..end], execution)?;
        let mut entry = [0u8; ENTRY];
        for (field, value) in [
            position as u32,
            offset as u32,
            (end - offset) as u32,
            plan.start,
            plan.count,
        ]
        .into_iter()
        .enumerate()
        {
            entry[field * 4..field * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        entry[20..52].copy_from_slice(&data_hash);
        let checksum = index_hash(&header_hash, &entry[..52]);
        entry[52..].copy_from_slice(&checksum);
        output
            [FIXED + bytes.len() + position * ENTRY..FIXED + bytes.len() + (position + 1) * ENTRY]
            .copy_from_slice(&entry);
    }
    output[..8].copy_from_slice(MAGIC);
    output[8..12].copy_from_slice(&VERSION.to_le_bytes());
    output[12..16].copy_from_slice(&(file_bytes as u32).to_le_bytes());
    output[16..24].copy_from_slice(&generation.to_le_bytes());
    for (offset, value) in [
        (24, bytes.len() as u32),
        (28, header.page_count),
        (32, header.page_size),
        (36, total),
    ] {
        output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    output[40..72].copy_from_slice(&header_hash);
    output[72..104].copy_from_slice(&hash(options.cursor_prefix.as_bytes(), execution)?);
    let checksum = hash(&output[..104], execution)?;
    output[104..FIXED].copy_from_slice(&checksum);
    execution.check()?;
    Ok(output)
}

fn block(
    reader: &mut (impl Read + Seek),
    offset: u64,
    bytes: usize,
    execution: Execution,
) -> Result<Vec<u8>> {
    execution.check()?;
    let positioned = reader.seek(SeekFrom::Start(offset));
    execution.check()?;
    positioned?;
    let mut output = vec![0; bytes];
    let mut filled = 0;
    while filled < bytes {
        execution.check()?;
        let read = reader.read(&mut output[filled..bytes.min(filled + 16 * 1024)]);
        execution.check()?;
        match read {
            Ok(0) => return Err(Error::CursorInvalidated),
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(Error::CursorInvalidated);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(output)
}
fn decoded<T: DeserializeOwned>(bytes: &[u8], execution: Execution) -> Result<T> {
    let result = serde_json::from_slice(bytes);
    execution.check()?;
    result.map_err(|_| Error::CursorInvalidated)
}
fn number(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn preamble(
    reader: &mut (impl Read + Seek),
    cursor_prefix: &str,
    generation: u64,
    execution: Execution,
) -> Result<Preamble> {
    execution.check()?;
    if cursor_prefix.is_empty() || cursor_prefix.len() > MAX_PREFIX || generation == 0 {
        return Err(Error::CursorInvalidated);
    }
    let length = reader.seek(SeekFrom::End(0));
    execution.check()?;
    let length = length?;
    if !(FIXED as u64..=super::MAX_RESULT_BYTES).contains(&length) {
        return Err(Error::CursorInvalidated);
    }
    let bytes = block(reader, 0, FIXED, execution)?;
    if &bytes[..8] != MAGIC
        || number(&bytes, 8) != VERSION
        || bytes[104..] != hash(&bytes[..104], execution)?
        || number(&bytes, 12) as u64 != length
        || u64::from_le_bytes(bytes[16..24].try_into().unwrap()) != generation
        || bytes[72..104] != hash(cursor_prefix.as_bytes(), execution)?
    {
        return Err(Error::CursorInvalidated);
    }
    let result = Preamble {
        bytes: number(&bytes, 12),
        generation,
        header_len: number(&bytes, 24),
        page_count: number(&bytes, 28),
        page_size: number(&bytes, 32),
        total: number(&bytes, 36),
        header_hash: bytes[40..72].try_into().unwrap(),
    };
    if result.header_len as usize > MAX_HEADER
        || result.header_len == 0
        || !(1..=1000).contains(&result.page_size)
        || result.page_count == 0
        || result.page_count > result.total.max(1)
        || FIXED as u64 + u64::from(result.header_len) + u64::from(result.page_count) * ENTRY as u64
            > length
    {
        return Err(Error::CursorInvalidated);
    }
    Ok(result)
}
fn entry(
    reader: &mut (impl Read + Seek),
    preamble: &Preamble,
    position: u32,
    execution: Execution,
) -> Result<Entry> {
    if position >= preamble.page_count {
        return Err(Error::CursorInvalidated);
    }
    let offset = FIXED as u64 + u64::from(preamble.header_len) + u64::from(position) * ENTRY as u64;
    let bytes = block(reader, offset, ENTRY, execution)?;
    if number(&bytes, 0) != position
        || bytes[52..] != index_hash(&preamble.header_hash, &bytes[..52])
    {
        return Err(Error::CursorInvalidated);
    }
    let result = Entry {
        offset: number(&bytes, 4),
        bytes: number(&bytes, 8),
        plan: Plan {
            start: number(&bytes, 12),
            count: number(&bytes, 16),
        },
        data_hash: bytes[20..52].try_into().unwrap(),
    };
    let data_start = FIXED as u64
        + u64::from(preamble.header_len)
        + u64::from(preamble.page_count) * ENTRY as u64;
    let end = u64::from(result.offset) + u64::from(result.bytes);
    let item_end = u64::from(result.plan.start) + u64::from(result.plan.count);
    if u64::from(result.offset) < data_start
        || result.bytes < 2
        || result.bytes > execution::MAX_RESPONSE_BYTES
        || end > u64::from(preamble.bytes)
        || result.plan.count > preamble.page_size
        || item_end > u64::from(preamble.total)
        || (position == 0 && (result.plan.start != 0 || u64::from(result.offset) != data_start))
        || (position > 0 && result.plan.start == 0)
        || (preamble.total > 0 && result.plan.count == 0)
        || (position + 1 == preamble.page_count
            && (item_end != u64::from(preamble.total) || end != u64::from(preamble.bytes)))
        || (position + 1 < preamble.page_count && item_end >= u64::from(preamble.total))
    {
        return Err(Error::CursorInvalidated);
    }
    Ok(result)
}
fn selected_entry(
    reader: &mut (impl Read + Seek),
    preamble: &Preamble,
    position: u32,
    execution: Execution,
) -> Result<Entry> {
    let selected = entry(reader, preamble, position, execution)?;
    if position > 0 {
        let previous = entry(reader, preamble, position - 1, execution)?;
        if previous.plan.start + previous.plan.count != selected.plan.start
            || previous.offset + previous.bytes != selected.offset
        {
            return Err(Error::CursorInvalidated);
        }
    }
    if position + 1 < preamble.page_count {
        let next = entry(reader, preamble, position + 1, execution)?;
        if selected.plan.start + selected.plan.count != next.plan.start
            || selected.offset + selected.bytes != next.offset
        {
            return Err(Error::CursorInvalidated);
        }
    }
    Ok(selected)
}

/// Check token position/snapshot binding without parsing a response header or
/// reading an unrelated graph page under a source/context response budget.
pub(crate) fn validate(
    mut reader: impl Read + Seek,
    position: u32,
    cursor_prefix: &str,
    expected_generation: u64,
    execution: Execution,
) -> Result<()> {
    let preamble = preamble(&mut reader, cursor_prefix, expected_generation, execution)?;
    selected_entry(&mut reader, &preamble, position, execution)?;
    execution.check()
}

pub(crate) fn read(
    mut reader: impl Read + Seek,
    options: ReadOptions<'_>,
    execution: Execution,
) -> Result<Value> {
    execution.check()?;
    limit(options.response_max_bytes)?;
    let preamble = preamble(
        &mut reader,
        options.cursor_prefix,
        options.expected_generation,
        execution,
    )?;
    let selected = selected_entry(&mut reader, &preamble, options.position, execution)?;
    let bytes = block(
        &mut reader,
        FIXED as u64,
        preamble.header_len as usize,
        execution,
    )?;
    if hash(&bytes, execution)? != preamble.header_hash {
        return Err(Error::CursorInvalidated);
    }
    let mut header: Header = decoded(&bytes, execution)?;
    let fields = names(&header.op).map_err(|_| Error::CursorInvalidated)?;
    if header.cursor_prefix != options.cursor_prefix
        || header.page_count != preamble.page_count
        || header.page_size != preamble.page_size
        || !(1..=1000).contains(&header.total_limit)
        || header.sections.len() != fields.len()
        || header
            .sections
            .iter()
            .zip(fields)
            .any(|(section, name)| section.name != *name)
        || header
            .sections
            .iter()
            .map(|section| u64::from(section.total))
            .sum::<u64>()
            != u64::from(preamble.total)
        || header.sections[0].total > header.total_limit
        || header.result.get("generation").and_then(Value::as_u64) != Some(preamble.generation)
        || !header
            .result
            .get("truncated")
            .is_some_and(Value::is_boolean)
        || !header.result.get("freshness").is_some_and(Value::is_string)
        || header.result.get("page").is_some()
    {
        return Err(Error::CursorInvalidated);
    }
    {
        let target =
            arrays(&mut header.result, &header.op).map_err(|_| Error::CursorInvalidated)?;
        if fields.iter().any(|name| {
            target
                .get(*name)
                .and_then(Value::as_array)
                .is_none_or(|rows| !rows.is_empty())
        }) {
            return Err(Error::CursorInvalidated);
        }
    }
    let bytes = block(
        &mut reader,
        u64::from(selected.offset),
        selected.bytes as usize,
        execution,
    )?;
    if hash(&bytes, execution)? != selected.data_hash {
        return Err(Error::CursorInvalidated);
    }
    let rows: Vec<Value> = decoded(&bytes, execution)?;
    if rows.len() != selected.plan.count as usize {
        return Err(Error::CursorInvalidated);
    }
    let metadata = encoded(
        &page(&header, options.position, selected.plan),
        execution,
        MAX_HEADER,
    )?;
    let metadata: Value = serde_json::from_slice(&metadata)?;
    let counts: Vec<usize> = {
        let ranges = progress(&header, selected.plan);
        fields
            .iter()
            .map(|field| ranges[*field].count as usize)
            .collect()
    };
    let mut rows = rows.into_iter();
    let target = arrays(&mut header.result, &header.op).map_err(|_| Error::CursorInvalidated)?;
    for (field, count) in fields.iter().zip(counts) {
        execution.check()?;
        target.insert(
            (*field).into(),
            Value::Array(rows.by_ref().take(count).collect()),
        );
    }
    let root = header
        .result
        .as_object_mut()
        .ok_or(Error::CursorInvalidated)?;
    root.insert("page".into(), metadata);
    root.insert("freshness".into(), Value::String(options.freshness.into()));
    root.remove("coordination");
    if let Some(coordination) = options.coordination {
        let bytes = encoded(coordination, execution, options.response_max_bytes)?;
        root.insert("coordination".into(), serde_json::from_slice(&bytes)?);
    }
    execution::check_response(&header.result, execution, options.response_max_bytes)?;
    execution.check()?;
    Ok(header.result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;
    const PREFIX: &str = concat!(
        "kc1",
        "00000000000000000000000000000000",
        "11111111111111111111111111111111",
        "."
    );
    fn execution() -> Execution {
        Execution::new(Default::default(), Some(120_000)).unwrap()
    }
    fn options(op: &str, limit: u32, page_size: u32, maximum: usize) -> EncodeOptions<'_> {
        EncodeOptions {
            op,
            total_limit: limit,
            page_size,
            cursor_prefix: PREFIX,
            response_max_bytes: maximum,
        }
    }
    fn settings(position: u32, maximum: usize) -> ReadOptions<'static> {
        ReadOptions {
            position,
            cursor_prefix: PREFIX,
            expected_generation: 7,
            response_max_bytes: maximum,
            freshness: "current",
            coordination: None,
        }
    }
    fn result(field: &str, rows: Vec<Value>) -> Value {
        let mut result = json!({"generation":7,"freshness":"current","truncated":false,
            "coverage_scope":"relevant","workspace_coverage":{"compilations":1,"statuses":{"complete":1}},
            "coverage":[{"compilation_id":"opaque-context","status":"complete"}]});
        result[field] = Value::Array(rows);
        result
    }
    fn get(bytes: &[u8], position: u32, maximum: usize) -> Value {
        read(Cursor::new(bytes), settings(position, maximum), execution()).unwrap()
    }
    fn seal_preamble(bytes: &mut [u8]) {
        let digest = Sha256::digest(&bytes[..104]);
        bytes[104..FIXED].copy_from_slice(&digest);
    }
    fn seal_entry(bytes: &mut [u8], position: u32) {
        let offset = FIXED + number(bytes, 24) as usize + position as usize * ENTRY;
        let header: [u8; 32] = bytes[40..72].try_into().unwrap();
        let digest = index_hash(&header, &bytes[offset..offset + 52]);
        bytes[offset + 52..offset + ENTRY].copy_from_slice(&digest);
    }
    #[derive(Default)]
    struct Counted<'a> {
        inner: Cursor<&'a [u8]>,
        bytes: usize,
        reads: usize,
    }
    impl Read for Counted<'_> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            let count = self.inner.read(bytes)?;
            self.bytes += count;
            Ok(count)
        }
    }
    impl Seek for Counted<'_> {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[test]
    fn primary_sections_replay_without_consuming_or_repacking() {
        for (op, field) in [
            ("symbols", "symbols"),
            ("references", "occurrences"),
            ("symbol_context", "contexts"),
            ("coverage", "coverage"),
        ] {
            let rows: Vec<Value> = (0..5)
                .map(|index| json!({"id":format!("opaque-{index}"),"text":"😀\n\"é"}))
                .collect();
            let original = result(field, rows.clone());
            let bytes = encode(original.clone(), options(op, 5, 2, 4096), execution()).unwrap();
            let mut all = Vec::new();
            for position in 0..3 {
                let page = get(&bytes, position, 4096);
                assert_eq!(
                    page,
                    get(&bytes, position, execution::MAX_RESPONSE_BYTES as usize)
                );
                assert_eq!(page["page"]["cursor"], format!("{PREFIX}{position:08x}"));
                assert_eq!(page["page"]["page_count"], 3);
                assert_eq!(page["page"]["total_limit"], 5);
                assert_eq!(page["page"]["sections"][field]["total"], 5);
                assert_eq!(page["page"]["sections"][field]["start"], position * 2);
                assert_eq!(page["generation"], 7);
                assert_eq!(page["truncated"], false);
                if op != "coverage" {
                    assert_eq!(page["coverage"], original["coverage"]);
                }
                all.extend(page[field].as_array().unwrap().clone());
                if position == 2 {
                    assert!(page["page"]["next_cursor"].is_null());
                } else {
                    assert_eq!(
                        page["page"]["next_cursor"],
                        format!("{PREFIX}{:08x}", position + 1)
                    );
                }
            }
            assert_eq!(all, rows);
            assert!(matches!(
                validate(Cursor::new(&bytes), 3, PREFIX, 7, execution()),
                Err(Error::CursorInvalidated)
            ));
        }
    }
    #[test]
    fn graph_findings_paths_and_later_boundaries_keep_partial_evidence() {
        let path = json!([{"source_id":"root","target_id":"middle","compilation_id":"proof","location":{"start_byte":0,"end_byte":4}},
            {"source_id":"middle","target_id":"affected","compilation_id":"implementation","resolution":"possible_dispatch"}]);
        let findings = vec![
            json!({"symbol":{"id":"affected"},"paths":[path]}),
            json!({"symbol":{"id":"other"},"paths":[[]]}),
        ];
        let boundaries: Vec<Value> = (0..3)
            .map(|index| json!({"reason":"external","symbol_id":format!("boundary-{index}")}))
            .collect();
        let graph = json!({"findings":findings,"boundaries":boundaries,"limits":["node_budget"],"examined_edges":97,"visited_nodes":12,"root_definitions":[{"definition_symbol_id":"root-body"}],"capability_coverage":{"calls":"partial"}});
        let mut input = result("symbols", vec![]);
        input["truncated"] = json!(true);
        input["graph"] = graph.clone();
        let bytes = encode(input, options("impact", 2, 2, 4096), execution()).unwrap();
        let mut found = Vec::new();
        let mut bounded = Vec::new();
        for position in 0..3 {
            let page = get(&bytes, position, 4096);
            assert_eq!(page["truncated"], true);
            assert_eq!(page["graph"]["limits"], graph["limits"]);
            assert_eq!(page["graph"]["examined_edges"], 97);
            assert_eq!(page["graph"]["root_definitions"], graph["root_definitions"]);
            assert_eq!(page["page"]["sections"]["findings"]["total"], 2);
            assert_eq!(page["page"]["sections"]["boundaries"]["total"], 3);
            found.extend(page["graph"]["findings"].as_array().unwrap().clone());
            bounded.extend(page["graph"]["boundaries"].as_array().unwrap().clone());
            if position == 0 {
                assert_eq!(page["page"]["sections"]["boundaries"]["count"], 0);
            }
            if position == 2 {
                assert!(page["page"]["next_cursor"].is_null());
            }
        }
        assert_eq!(found, findings);
        assert_eq!(bounded, boundaries);
    }
    #[test]
    fn exact_terminal_envelope_and_short_terminal_tail_fit() {
        let input = result("symbols", vec![json!({"signature":"é\"\n".repeat(500)})]);
        let large = encode(input.clone(), options("symbols", 1, 1, 16_384), execution()).unwrap();
        let page = get(&large, 0, 16_384);
        let exact = json!({"result":page}).to_string().len();
        assert!(exact >= 1024);
        let bytes = encode(input.clone(), options("symbols", 1, 1, exact), execution()).unwrap();
        assert_eq!(get(&bytes, 0, exact), page);
        assert!(matches!(
            encode(input, options("symbols", 1, 1, exact - 1), execution()),
            Err(Error::ResponseLimitExceeded)
        ));
        // The final null token can make the complete two-item tail fit even
        // when a one-item, nonterminal prefix would exceed this same budget.
        let input = result(
            "symbols",
            vec![json!({"signature":"x".repeat(1000)}), json!({"id":"x"})],
        );
        let large = encode(input.clone(), options("symbols", 2, 2, 4096), execution()).unwrap();
        let terminal = get(&large, 0, 4096);
        let exact = json!({"result":terminal}).to_string().len();
        let bytes = encode(input, options("symbols", 2, 2, exact), execution()).unwrap();
        assert_eq!(
            get(&bytes, 0, exact)["symbols"].as_array().unwrap().len(),
            2
        );
    }
    #[test]
    fn frozen_pages_use_request_budget_after_metadata_replacement() {
        let input = result(
            "symbols",
            vec![
                json!({"body":"x".repeat(1300)}),
                json!({"body":"y".repeat(1300)}),
            ],
        );
        let bytes = encode(input, options("symbols", 2, 2, 2048), execution()).unwrap();
        let old = get(&bytes, 0, 2048);
        assert_eq!(old["symbols"].as_array().unwrap().len(), 1);
        assert!(matches!(
            read(Cursor::new(&bytes), settings(0, 1024), execution()),
            Err(Error::ResponseLimitExceeded)
        ));
        let bigger = get(&bytes, 0, 8192);
        assert_eq!(bigger, old);
        let status = WorkspaceStatus {
            generation: 8,
            phase: "idle".into(),
            error: "e".repeat(3000),
            ..Default::default()
        };
        let mut updated = settings(0, 2048);
        updated.freshness = "stale";
        updated.coordination = Some(&status);
        assert!(matches!(
            read(Cursor::new(&bytes), updated, execution()),
            Err(Error::ResponseLimitExceeded)
        ));
        let mut updated = settings(0, 8192);
        updated.freshness = "stale";
        updated.coordination = Some(&status);
        let current = read(Cursor::new(&bytes), updated, execution()).unwrap();
        assert_eq!(current["symbols"], old["symbols"]);
        assert_eq!(current["page"], old["page"]);
        assert_eq!(current["generation"], 7);
        assert_eq!(current["coordination"]["generation"], 8);
        assert_eq!(current["freshness"], "stale");
        assert_eq!(get(&bytes, 0, 8192), old);
    }
    #[test]
    fn atomic_context_path_and_required_header_cannot_be_stripped() {
        let input = result(
            "contexts",
            vec![
                json!({"declarations":[{"source":{"text":"x".repeat(3000),"content_hash":"exact-version"}}]}),
            ],
        );
        assert!(matches!(
            encode(input, options("symbol_context", 1, 1, 1024), execution()),
            Err(Error::ResponseLimitExceeded)
        ));
        let mut input = result("symbols", vec![]);
        input["graph"] = json!({"findings":[{"symbol":{"id":"x"},"paths":[[{"evidence":"x".repeat(3000)}]]}],"boundaries":[],"limits":[]});
        assert!(matches!(
            encode(input, options("callers", 1, 1, 1024), execution()),
            Err(Error::ResponseLimitExceeded)
        ));
        let mut input = result("symbols", vec![json!({"id":"tiny"})]);
        input["coverage"] = json!([{"required_evidence":"x".repeat(3000)}]);
        assert!(matches!(
            encode(input, options("symbols", 1, 1, 1024), execution()),
            Err(Error::ResponseLimitExceeded)
        ));
    }
    #[test]
    fn empty_results_still_have_one_replayable_page() {
        let bytes = encode(
            result("symbols", vec![]),
            options("symbols", 100, 20, 1024),
            execution(),
        )
        .unwrap();
        let page = get(&bytes, 0, 1024);
        assert_eq!(page["symbols"], json!([]));
        assert_eq!(page["page"]["page_count"], 1);
        assert_eq!(page["page"]["sections"]["symbols"]["total"], 0);
        assert!(page["page"]["next_cursor"].is_null());
        validate(Cursor::new(&bytes), 0, PREFIX, 7, execution()).unwrap();
        assert_eq!(get(&bytes, 0, 1024), page);
    }
    #[test]
    fn many_pages_keep_one_header_and_read_only_the_selected_slice() {
        let rows: Vec<Value> = (0..1000)
            .map(|index| json!({"id":index,"signature":"x".repeat(1000)}))
            .collect();
        let mut input = result("symbols", rows);
        input["coverage"] = json!([{"required_evidence":"q".repeat(50_000)}]);
        let bytes = encode(input, options("symbols", 1000, 1, 65_536), execution()).unwrap();
        assert!(bytes.len() < 2_000_000);
        let mut source = Counted {
            inner: Cursor::new(bytes.as_slice()),
            ..Default::default()
        };
        validate(&mut source, 999, PREFIX, 7, execution()).unwrap();
        assert!(source.bytes <= 512);
        let mut source = Counted {
            inner: Cursor::new(bytes.as_slice()),
            ..Default::default()
        };
        let page = read(&mut source, settings(999, 65_536), execution()).unwrap();
        assert!(source.bytes < bytes.len() / 4);
        assert_eq!(page["symbols"][0]["id"], 999);
        assert_eq!(page["page"]["page_count"], 1000);
        assert!(page["page"]["next_cursor"].is_null());
        assert_eq!(
            get(&bytes, 9, 65_536)["page"]["next_cursor"],
            format!("{PREFIX}0000000a")
        );
    }
    #[test]
    fn corrupted_and_out_of_range_components_invalidate_without_large_reads() {
        let original = encode(
            result("symbols", vec![json!({"id":"a"}), json!({"id":"b"})]),
            options("symbols", 2, 1, 1024),
            execution(),
        )
        .unwrap();
        for offset in [0, 8, 12, 16, 24, 28, 40, 72, 104, FIXED, original.len() - 1] {
            let mut bytes = original.clone();
            bytes[offset] ^= 1;
            assert!(
                matches!(
                    read(Cursor::new(&bytes), settings(1, 1024), execution()),
                    Err(Error::CursorInvalidated)
                ),
                "offset {offset}"
            );
        }
        let index = FIXED + number(&original, 24) as usize;
        for field in [4, 8, 12, 16] {
            let mut bytes = original.clone();
            bytes[index + field..index + field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            seal_entry(&mut bytes, 0);
            let mut source = Counted {
                inner: Cursor::new(bytes.as_slice()),
                ..Default::default()
            };
            assert!(matches!(
                read(&mut source, settings(0, 1024), execution()),
                Err(Error::CursorInvalidated)
            ));
            assert!(source.bytes <= 512);
        }
        for field in [24, 28] {
            let mut bytes = original.clone();
            bytes[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            seal_preamble(&mut bytes);
            let mut source = Counted {
                inner: Cursor::new(bytes.as_slice()),
                ..Default::default()
            };
            assert!(matches!(
                validate(&mut source, 0, PREFIX, 7, execution()),
                Err(Error::CursorInvalidated)
            ));
            assert!(source.bytes <= FIXED);
        }
        let wide = encode(
            result("symbols", (0..6).map(|id| json!({"id":id})).collect()),
            options("symbols", 6, 2, 1024),
            execution(),
        )
        .unwrap();
        let middle = FIXED + number(&wide, 24) as usize + ENTRY;
        for (field, value) in [(12, 3u32), (16, 1u32)] {
            let mut bytes = wide.clone();
            bytes[middle + field..middle + field + 4].copy_from_slice(&value.to_le_bytes());
            seal_entry(&mut bytes, 1);
            let mut source = Counted {
                inner: Cursor::new(bytes.as_slice()),
                ..Default::default()
            };
            assert!(matches!(
                validate(&mut source, 1, PREFIX, 7, execution()),
                Err(Error::CursorInvalidated)
            ));
            assert!(source.bytes <= 512);
        }
        for cut in [0, FIXED - 1, original.len() - 1] {
            assert!(matches!(
                read(
                    Cursor::new(&original[..cut]),
                    settings(0, 1024),
                    execution()
                ),
                Err(Error::CursorInvalidated)
            ));
        }
        assert!(matches!(
            validate(Cursor::new(&original), 0, PREFIX, 8, execution()),
            Err(Error::CursorInvalidated)
        ));
        assert!(matches!(
            validate(Cursor::new(&original), 0, "other.", 7, execution()),
            Err(Error::CursorInvalidated)
        ));
        assert!(matches!(
            validate(Cursor::new(&original), u32::MAX, PREFIX, 7, execution()),
            Err(Error::CursorInvalidated)
        ));
    }
    #[test]
    fn checksum_valid_malformed_json_or_inconsistent_header_is_invalid() {
        let original = encode(
            result("symbols", vec![json!({"id":"a"})]),
            options("symbols", 1, 1, 1024),
            execution(),
        )
        .unwrap();
        let index = FIXED + number(&original, 24) as usize;
        let start = number(&original, index + 4) as usize;
        let length = number(&original, index + 8) as usize;
        let mut bytes = original.clone();
        bytes[start] = b'{';
        let digest = Sha256::digest(&bytes[start..start + length]);
        bytes[index + 20..index + 52].copy_from_slice(&digest);
        seal_entry(&mut bytes, 0);
        assert!(matches!(
            read(Cursor::new(&bytes), settings(0, 1024), execution()),
            Err(Error::CursorInvalidated)
        ));
        let mut bytes = original.clone();
        let header_len = number(&bytes, 24) as usize;
        let mut header: Value = serde_json::from_slice(&bytes[FIXED..FIXED + header_len]).unwrap();
        header["result"]["generation"] = json!(8);
        let changed = serde_json::to_vec(&header).unwrap();
        assert_eq!(changed.len(), header_len);
        bytes[FIXED..FIXED + header_len].copy_from_slice(&changed);
        let digest = Sha256::digest(&changed);
        bytes[40..72].copy_from_slice(&digest);
        seal_preamble(&mut bytes);
        seal_entry(&mut bytes, 0);
        assert!(matches!(
            read(Cursor::new(&bytes), settings(0, 1024), execution()),
            Err(Error::CursorInvalidated)
        ));
    }
    #[test]
    fn invalid_scope_and_aggregate_file_size_fail_explicitly() {
        let input = result("symbols", vec![json!({"id":1}), json!({"id":2})]);
        let mut missing = input.clone();
        missing.as_object_mut().unwrap().remove("freshness");
        assert!(matches!(
            encode(missing, options("symbols", 2, 1, 1024), execution()),
            Err(Error::InvalidRequest(_))
        ));
        assert!(matches!(
            encode(input.clone(), options("symbols", 1, 1, 1024), execution()),
            Err(Error::InvalidRequest(_))
        ));
        assert!(matches!(
            encode(
                input.clone(),
                options("references", 2, 1, 1024),
                execution()
            ),
            Err(Error::InvalidRequest(_))
        ));
        assert!(matches!(
            encode(input.clone(), options("source", 2, 1, 1024), execution()),
            Err(Error::InvalidRequest(_))
        ));
        assert!(matches!(
            encode(input, options("symbols", 2, 0, 1024), execution()),
            Err(Error::InvalidRequest(_))
        ));
        let rows = (0..1000)
            .map(|_| json!({"body":"x".repeat(20_000)}))
            .collect();
        assert!(matches!(
            encode(
                result("symbols", rows),
                options(
                    "symbols",
                    1000,
                    1000,
                    execution::MAX_RESPONSE_BYTES as usize
                ),
                execution()
            ),
            Err(Error::ResourceLimited(_))
        ));
    }
    #[test]
    fn expiry_stops_before_io_and_between_trickled_reads() {
        let bytes = encode(
            result("symbols", vec![]),
            options("symbols", 1, 1, 1024),
            execution(),
        )
        .unwrap();
        let expired = Execution::new(Default::default(), Some(0)).unwrap();
        assert!(matches!(
            encode(
                result("symbols", vec![]),
                options("symbols", 1, 1, 1024),
                expired
            ),
            Err(Error::DeadlineExceeded)
        ));
        let mut source = Counted {
            inner: Cursor::new(bytes.as_slice()),
            ..Default::default()
        };
        assert!(matches!(
            read(&mut source, settings(0, 1024), expired),
            Err(Error::DeadlineExceeded)
        ));
        assert_eq!(source.reads, 0);
        struct Trickle<'a>(Cursor<&'a [u8]>, usize);
        impl Read for Trickle<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.1 += 1;
                std::thread::sleep(std::time::Duration::from_millis(15));
                self.0.read(&mut bytes[..1])
            }
        }
        impl Seek for Trickle<'_> {
            fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
                self.0.seek(position)
            }
        }
        let mut source = Trickle(Cursor::new(&bytes), 0);
        assert!(matches!(
            validate(
                &mut source,
                0,
                PREFIX,
                7,
                Execution::new(Default::default(), Some(5)).unwrap()
            ),
            Err(Error::DeadlineExceeded)
        ));
        assert_eq!(source.1, 1);
    }
}
