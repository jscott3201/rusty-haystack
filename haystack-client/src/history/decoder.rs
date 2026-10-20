use super::stream::{
    Control, HistoryStreamBatch, HistoryStreamEnd, HistoryStreamError as Error,
    HistoryStreamLimit as Limit, HistoryStreamLimits, Stats,
};
use haystack_core::{
    codecs::{
        history::{
            self as wire, HistoryReadHeader, HistoryReadRequest, HistoryReadValidator,
            HistorySample,
        },
        zinc::ZincParser,
    },
    data::{HCol, HDict, HGrid},
    kinds::Kind,
};
use std::sync::{Arc, atomic::Ordering};

pub(super) struct Decoder {
    reader: Reader,
    limits: HistoryStreamLimits,
    control: Control,
    stats: Arc<Stats>,
    validator: HistoryReadValidator,
    pending: Option<HistorySample>,
    rows: usize,
    source_bytes: u64,
}
struct Reader {
    response: reqwest::Response,
    frame: Vec<u8>,
    position: usize,
    received: usize,
    work: u64,
}
impl Reader {
    async fn line(
        &mut self,
        max: usize,
        kind: Limit,
        limits: &HistoryStreamLimits,
        control: &Control,
        stats: &Stats,
    ) -> Result<Option<Vec<u8>>, Error> {
        let mut line = Vec::new();
        loop {
            control.check()?;
            if self.position == self.frame.len() {
                // Drop the previous allocation before polling the next frame.
                self.frame = Vec::new();
                self.position = 0;
                let chunk = control
                    .run(self.response.chunk())
                    .await?
                    .map_err(|_| Error::Transport)?;
                let Some(chunk) = chunk else {
                    return if line.is_empty() {
                        Ok(None)
                    } else {
                        Err(Error::Protocol)
                    };
                };
                if chunk.len() > limits.frame_bytes {
                    return Err(Error::Limit(Limit::FrameBytes));
                }
                if chunk.len() > limits.response_bytes.saturating_sub(self.received) {
                    return Err(Error::Limit(Limit::ResponseBytes));
                }
                self.received += chunk.len();
                self.work = self
                    .work
                    .saturating_add((chunk.len() as u64).saturating_mul(4));
                if self.work > limits.work {
                    return Err(Error::Limit(Limit::Work));
                }
                stats
                    .received
                    .store(self.received as u64, Ordering::Relaxed);
                stats.work.store(self.work, Ordering::Relaxed);
                stats.frame.fetch_max(chunk.len() as u64, Ordering::Relaxed);
                self.frame = chunk.to_vec();
                // Empty HTTP DATA frames cannot prevent cancellation or starve
                // another task; body byte limits alone do not bound their count.
                tokio::task::yield_now().await;
                continue;
            }
            let remaining = &self.frame[self.position..];
            let newline = remaining.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(remaining.len(), |index| index + 1);
            if take > max.saturating_sub(line.len()) {
                return Err(Error::Limit(kind));
            }
            line.try_reserve_exact(take)
                .map_err(|_| Error::Limit(kind))?;
            line.extend_from_slice(&remaining[..take]);
            self.position += take;
            stats.line.fetch_max(line.len() as u64, Ordering::Relaxed);
            if newline.is_some() {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                control.check()?;
                return Ok(Some(line));
            }
        }
    }
}
impl Decoder {
    pub async fn open(
        response: reqwest::Response,
        request: &HistoryReadRequest,
        mut limits: HistoryStreamLimits,
        control: Control,
        stats: Arc<Stats>,
    ) -> Result<Self, Error> {
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            return Err(Error::Credentials);
        }
        if status != 200 {
            return Err(Error::Http(status));
        }
        let headers = response.headers();
        let mut types = headers.get_all("content-type").iter();
        if types.next().is_none_or(|v| v != "text/zinc") || types.next().is_some() {
            return Err(Error::Protocol);
        }
        let mut encodings = headers.get_all("content-encoding").iter();
        if encodings.next().is_some_and(|v| v != "identity") || encodings.next().is_some() {
            return Err(Error::Protocol);
        }
        if response
            .content_length()
            .is_some_and(|n| n > limits.response_bytes as u64)
        {
            return Err(Error::Limit(Limit::ResponseBytes));
        }
        let mut reader = Reader {
            response,
            frame: vec![],
            position: 0,
            received: 0,
            work: 0,
        };
        let header = reader
            .line(
                limits.header_bytes,
                Limit::HeaderBytes,
                &limits,
                &control,
                &stats,
            )
            .await?
            .ok_or(Error::Protocol)?;
        let columns = reader
            .line(16, Limit::HeaderBytes, &limits, &control, &stats)
            .await?
            .ok_or(Error::Protocol)?;
        if columns != b"ts,val" {
            return Err(Error::Protocol);
        }
        let header = parse_header(&header)?;
        let metadata = &header.metadata;
        if [&metadata.start, &metadata.end, &metadata.evaluated_at]
            .into_iter()
            .chain(metadata.coverage.retained_start.iter())
            .chain(metadata.coverage.retained_end.iter())
            .chain(metadata.coverage.evicted_through.iter())
            .any(|time| !haystack_core::codecs::shared::h4_datetime_representable(&time.dt))
        {
            return Err(Error::Protocol);
        }
        header
            .validate_for_request(request)
            .map_err(|_| Error::Protocol)?;
        limits.response_bytes = limits
            .response_bytes
            .min(header.metadata.bounds.response_bytes as usize);
        limits.batch_rows = limits
            .batch_rows
            .min(header.metadata.bounds.batch_rows as usize);
        limits.batch_bytes = limits
            .batch_bytes
            .min(header.metadata.bounds.batch_bytes as usize);
        if reader.received > limits.response_bytes {
            return Err(Error::Limit(Limit::ResponseBytes));
        }
        let validator = HistoryReadValidator::new(header).map_err(|_| Error::Protocol)?;
        control.check()?;
        Ok(Self {
            reader,
            limits,
            control,
            stats,
            validator,
            pending: None,
            rows: 0,
            source_bytes: 0,
        })
    }
    pub fn header(&self) -> &HistoryReadHeader {
        self.validator.header()
    }
    async fn line(&mut self) -> Result<Option<Vec<u8>>, Error> {
        self.reader
            .line(
                self.limits.row_bytes,
                Limit::RowBytes,
                &self.limits,
                &self.control,
                &self.stats,
            )
            .await
    }
    async fn sample(&mut self) -> Result<Option<HistorySample>, Error> {
        if let Some(sample) = self.pending.take() {
            return Ok(Some(sample));
        }
        let line = match self.line().await? {
            Some(line) => line,
            None => return Ok(None),
        };
        if line.is_empty() {
            // One final empty line is permitted; data or another blank line
            // after it is trailing garbage, not another grid.
            if self.line().await?.is_some() {
                return Err(Error::Protocol);
            }
            return Ok(None);
        }
        if self.rows >= self.limits.rows {
            return Err(Error::Limit(Limit::Rows));
        }
        let text = std::str::from_utf8(&line).map_err(|_| Error::Protocol)?;
        let (ts, val) = text.split_once(',').ok_or(Error::Protocol)?;
        if ts.len() > 256 {
            return Err(Error::Limit(Limit::FieldBytes));
        }
        scalar_preflight(ts, 256)?;
        scalar_preflight(val, self.limits.field_bytes)?;
        let Kind::DateTime(ts) = ZincParser::new(ts)
            .parse_scalar()
            .map_err(|_| Error::Protocol)?
        else {
            return Err(Error::Protocol);
        };
        let val = ZincParser::new(val)
            .parse_scalar()
            .map_err(|_| Error::Protocol)?;
        let sample = HistorySample { ts, val };
        if !haystack_core::codecs::shared::h4_datetime_representable(&sample.ts.dt) {
            return Err(Error::Protocol);
        }
        let bytes = HistoryReadValidator::sample_bytes(&sample);
        if self.source_bytes.saturating_add(bytes) > self.limits.source_bytes as u64 {
            return Err(Error::Limit(Limit::SourceBytes));
        }
        if self.reader.work.saturating_add(bytes) > self.limits.work {
            return Err(Error::Limit(Limit::Work));
        }
        self.validator
            .accept(&sample)
            .map_err(|_| Error::Protocol)?;
        self.source_bytes += bytes;
        self.reader.work += bytes;
        self.rows += 1;
        self.stats.rows.store(self.rows as u64, Ordering::Relaxed);
        self.stats.work.store(self.reader.work, Ordering::Relaxed);
        self.control.check()?;
        Ok(Some(sample))
    }
    pub async fn next_batch(&mut self) -> HistoryStreamBatch {
        let mut samples = Vec::with_capacity(self.limits.batch_rows);
        let mut bytes = 0;
        let outcome = loop {
            if let Err(error) = self.control.check() {
                break Some(HistoryStreamEnd::Local(error));
            }
            if samples.len() >= self.limits.batch_rows {
                break None;
            }
            match self.sample().await {
                Ok(Some(sample)) => {
                    let size = HistoryReadValidator::sample_bytes(&sample) as usize;
                    if size > self.limits.batch_bytes {
                        break Some(HistoryStreamEnd::Local(Error::Limit(Limit::BatchBytes)));
                    }
                    if size > self.limits.batch_bytes.saturating_sub(bytes) {
                        self.pending = Some(sample);
                        break None;
                    }
                    bytes += size;
                    samples.push(sample);
                    self.stats.batch.fetch_max(bytes as u64, Ordering::Relaxed);
                    self.stats
                        .batch_rows
                        .fetch_max(samples.len() as u64, Ordering::Relaxed);
                }
                Ok(None) => {
                    break Some(match self.validator.finish() {
                        Ok(terminal) => HistoryStreamEnd::Remote(terminal),
                        Err(_) => HistoryStreamEnd::Local(Error::Protocol),
                    });
                }
                Err(error) => break Some(HistoryStreamEnd::Local(error)),
            }
            // Parsing is finite and synchronous for one bounded row. Yield
            // after every row so stop/deadline also run on a single-thread RT.
            tokio::task::yield_now().await;
        };
        HistoryStreamBatch {
            samples,
            end: outcome,
        }
    }
}
fn parse_header(bytes: &[u8]) -> Result<HistoryReadHeader, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Protocol)?;
    let mut remaining = text.strip_prefix("ver:\"3.0\" ").ok_or(Error::Protocol)?;
    let mut meta = HDict::new();
    while !remaining.is_empty() {
        let (name, rest) = remaining.split_once(':').ok_or(Error::Protocol)?;
        if !matches!(name, "history" | "id" | "hisStart" | "hisEnd") || meta.has(name) {
            return Err(Error::Protocol);
        }
        // Reject nested values before the generic scalar decoder. The only
        // compound data is a bounded typed control encoded INSIDE a string.
        let value_len = scalar_prefix(rest)?;
        let source = &rest[..value_len];
        scalar_preflight(
            source,
            if name == "history" {
                wire::MAX_CONTROL_BYTES
            } else {
                512
            },
        )?;
        let value = ZincParser::new(source)
            .parse_scalar()
            .map_err(|_| Error::Protocol)?;
        meta.set(name, value);
        remaining = &rest[value_len..];
        if !remaining.is_empty() {
            if !remaining.starts_with(' ') {
                return Err(Error::Protocol);
            }
            remaining = remaining.trim_start_matches(' ');
        }
    }
    let grid = HGrid::from_parts(meta, vec![HCol::new("ts"), HCol::new("val")], vec![]);
    wire::result_header_from_grid(&grid).map_err(|_| Error::Protocol)
}
fn scalar_prefix(source: &str) -> Result<usize, Error> {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in source.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
        } else if c == '"' {
            quoted = true;
        } else if matches!(c, '[' | ']' | '{' | '}' | '(' | ')' | '`' | '<' | '>') {
            return Err(Error::Protocol);
        } else if c == ' ' {
            // DateTime timezone follows a space; metadata names end in ':'.
            let tail = &source[i + 1..];
            if tail
                .split(' ')
                .next()
                .is_some_and(|word| word.contains(':'))
            {
                return Ok(i);
            }
        }
    }
    if quoted || escaped {
        return Err(Error::Protocol);
    }
    Ok(source.len())
}
fn scalar_preflight(source: &str, max: usize) -> Result<(), Error> {
    // Record CRLF was removed by Reader. Any remaining raw line break belongs
    // to a field, where only escaped \r and \n are admitted.
    if source.contains(['\r', '\n']) {
        return Err(Error::Protocol);
    }
    let source = source.trim();
    if source.is_empty() {
        return Err(Error::Protocol);
    }
    if !source.starts_with('"') {
        if source.len() > max {
            return Err(Error::Limit(Limit::FieldBytes));
        }
        // Zinc's Date/Time/DateTime readers advance fixed byte counts. Their
        // syntax (including the timezone name) is ASCII, so exclude Unicode
        // before those paths can step into a code point. Check every scalar,
        // including a wrong-kind value, before its later semantic rejection.
        // Numeric units remain Unicode; e.g. 1.2e-3°F is not temporal syntax.
        let bytes = source.as_bytes();
        let temporal = [(4, b'-'), (2, b':')]
            .into_iter()
            .any(|(digits, separator)| {
                bytes.get(digits) == Some(&separator)
                    && bytes
                        .get(..digits)
                        .is_some_and(|prefix| prefix.iter().all(u8::is_ascii_digit))
            });
        if temporal && !source.is_ascii() {
            return Err(Error::Protocol);
        }
        if source.bytes().any(|b| {
            matches!(
                b,
                b'[' | b']' | b'{' | b'}' | b'(' | b')' | b'`' | b'<' | b'>' | b'"' | b'\\'
            )
        }) {
            return Err(Error::Protocol);
        }
        return Ok(());
    }
    let mut chars = source[1..].char_indices();
    let mut bytes = 0;
    while let Some((index, c)) = chars.next() {
        if c == '"' {
            return if index + 2 == source.len() {
                Ok(())
            } else {
                Err(Error::Protocol)
            };
        }
        let size = if c == '\\' {
            match chars.next().map(|(_, c)| c).ok_or(Error::Protocol)? {
                'u' => {
                    let mut code = 0u32;
                    for _ in 0..4 {
                        code = (code << 4)
                            | chars
                                .next()
                                .and_then(|(_, c)| c.to_digit(16))
                                .ok_or(Error::Protocol)?;
                    }
                    char::from_u32(code).ok_or(Error::Protocol)?.len_utf8()
                }
                '"' | '\\' | 'n' | 'r' | 't' | 'b' | 'f' | '$' => 1,
                _ => return Err(Error::Protocol),
            }
        } else {
            c.len_utf8()
        };
        bytes += size;
        if bytes > max {
            return Err(Error::Limit(Limit::FieldBytes));
        }
    }
    Err(Error::Protocol)
}
