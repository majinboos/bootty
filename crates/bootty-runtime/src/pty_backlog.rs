use std::{collections::VecDeque, time::Instant};

const MAX_DRAIN_BYTES: usize = 4 * 1024 * 1024;
const MAX_DRAIN_CHUNKS: usize = 32;
const MAX_DRAIN_SLICE_BYTES: usize = 8 * 1024;
const MAX_DRAIN_TIME_US: u128 = 20_000;

#[derive(Clone, Debug, Default)]
pub struct OutputBacklog {
    chunks: VecDeque<Vec<u8>>,
    bytes: usize,
    front_offset: usize,
}

impl OutputBacklog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            chunks: VecDeque::with_capacity(capacity),
            bytes: 0,
            front_offset: 0,
        }
    }

    pub fn push_back(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.chunks.push_back(bytes);
    }

    pub fn len(&self) -> usize {
        self.bytes
    }

    pub fn is_empty(&self) -> bool {
        self.bytes == 0
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.bytes = 0;
        self.front_offset = 0;
    }

    pub(crate) fn front_len(&self) -> Option<usize> {
        self.chunks
            .front()
            .map(|front| front.len().saturating_sub(self.front_offset))
    }

    pub(crate) fn consume_front(&mut self, len: usize, mut consume: impl FnMut(&[u8])) {
        let end = self.front_offset + len;
        if let Some(front) = self.chunks.front() {
            consume(&front[self.front_offset..end]);
        }

        self.front_offset = end;
        self.bytes = self.bytes.saturating_sub(len);
        if self
            .chunks
            .front()
            .is_some_and(|front| self.front_offset >= front.len())
        {
            self.chunks.pop_front();
            self.front_offset = 0;
        }
    }
}

pub fn drain_output_backlog(backlog: &mut OutputBacklog, write: impl FnMut(&[u8])) -> DrainStats {
    drain_output_backlog_with_limits(
        backlog,
        MAX_DRAIN_BYTES,
        MAX_DRAIN_CHUNKS,
        MAX_DRAIN_TIME_US,
        write,
    )
}

pub fn drain_output_backlog_with_limits(
    backlog: &mut OutputBacklog,
    max_bytes: usize,
    max_chunks: usize,
    max_time_us: u128,
    mut write: impl FnMut(&[u8]),
) -> DrainStats {
    let start = Instant::now();
    let mut stats = DrainStats::default();

    while !backlog.is_empty()
        && !drain_budget_exhausted_with_limits(stats, max_bytes, max_chunks)
        && !drain_time_exhausted(start, max_time_us)
    {
        let Some(available) = backlog.front_len() else {
            backlog.clear();
            break;
        };
        let consumed = drain_slice_len_with_limit(stats, max_bytes, available);
        if consumed == 0 {
            break;
        }

        stats.chunks += 1;
        backlog.consume_front(consumed, |bytes| write(bytes));
        stats.bytes += consumed;
    }

    stats.elapsed_us = start.elapsed().as_micros() as u64;
    stats
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrainStats {
    pub chunks: usize,
    pub bytes: usize,
    pub elapsed_us: u64,
}

fn drain_bytes_remaining_with_limit(stats: DrainStats, max_bytes: usize) -> usize {
    max_bytes.saturating_sub(stats.bytes)
}

fn drain_slice_len_with_limit(stats: DrainStats, max_bytes: usize, available: usize) -> usize {
    drain_bytes_remaining_with_limit(stats, max_bytes)
        .min(MAX_DRAIN_SLICE_BYTES)
        .min(available)
}

fn drain_time_exhausted(start: Instant, max_time_us: u128) -> bool {
    start.elapsed().as_micros() >= max_time_us
}

fn drain_budget_exhausted_with_limits(
    stats: DrainStats,
    max_bytes: usize,
    max_chunks: usize,
) -> bool {
    stats.bytes >= max_bytes || stats.chunks >= max_chunks
}

pub use OutputBacklog as PtyBacklog;
pub use drain_output_backlog as drain_pty_backlog;
pub(crate) use drain_output_backlog_with_limits as drain_pty_backlog_with_limits;
