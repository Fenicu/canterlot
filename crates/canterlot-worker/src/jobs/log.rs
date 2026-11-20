//! Журнал вывода попытки, ограниченный на диске: голова, отметка пропуска и скользящий хвост.
//!
//! Пишет его один процесс — оболочка попытки. Читает воркер: для кусков живого вывода,
//! для итога (хвост в факте) и для архива, который уходит на хаб.
//!
//! Раскладка: `log.head` — первые `head_cap` байт потока; дальше хвост лежит сегментами
//! `log.t.<начало>`, где начало — позиция сегмента в потоке вывода. Живых сегментов не больше
//! двух: когда текущий дорос до `seg_cap`, начинается новый, а третий с конца удаляется.
//! Всё вместе занимает не больше `head_cap + 2 * seg_cap`.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

const HEAD: &str = "log.head";
const SEGMENT_PREFIX: &str = "log.t.";

/// Доли предела: голова и каждый из двух сегментов хвоста берут по четверти.
pub fn caps(log_max_bytes: u64) -> (u64, u64) {
    let part = (log_max_bytes / 4).max(1);
    (part, part)
}

pub struct LogWriter {
    dir: PathBuf,
    head_cap: u64,
    seg_cap: u64,
    head: File,
    head_len: u64,
    total: u64,
    current: Option<Segment>,
    previous_start: Option<u64>,
}

struct Segment {
    file: File,
    start: u64,
    len: u64,
}

impl LogWriter {
    pub fn create(dir: &Path, log_max_bytes: u64) -> io::Result<Self> {
        let (head_cap, seg_cap) = caps(log_max_bytes);
        Ok(Self {
            dir: dir.to_path_buf(),
            head_cap,
            seg_cap,
            head: open_append(&dir.join(HEAD))?,
            head_len: 0,
            total: 0,
            current: None,
            previous_start: None,
        })
    }

    /// Сколько байт вывода принято всего, включая выброшенные.
    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn write(&mut self, mut data: &[u8]) -> io::Result<()> {
        if self.head_len < self.head_cap {
            let room = usize::try_from(self.head_cap - self.head_len).unwrap_or(usize::MAX);
            let take = room.min(data.len());
            self.head.write_all(&data[..take])?;
            self.head_len += take as u64;
            self.total += take as u64;
            data = &data[take..];
        }
        while !data.is_empty() {
            if self
                .current
                .as_ref()
                .is_none_or(|segment| segment.len >= self.seg_cap)
            {
                self.rotate()?;
            }
            let segment = self.current.as_mut().expect("segment was just opened");
            let room = usize::try_from(self.seg_cap - segment.len).unwrap_or(usize::MAX);
            let take = room.min(data.len());
            segment.file.write_all(&data[..take])?;
            segment.len += take as u64;
            self.total += take as u64;
            data = &data[take..];
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let retired = self.current.take().map(|segment| segment.start);
        // Живы остаются прежний и новый; всё, что старше прежнего, удаляется.
        if let Some(older) = self.previous_start.take() {
            let _ = fs::remove_file(segment_path(&self.dir, older));
        }
        self.previous_start = retired;
        let start = self.total;
        self.current = Some(Segment {
            file: open_append(&segment_path(&self.dir, start))?,
            start,
            len: 0,
        });
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

fn segment_path(dir: &Path, start: u64) -> PathBuf {
    dir.join(format!("{SEGMENT_PREFIX}{start:020}"))
}

/// Что лежит на диске: читается воркером независимо от писателя.
pub struct LogView {
    head_path: PathBuf,
    head_len: u64,
    /// `(начало, длина, путь)` по возрастанию начала.
    segments: Vec<(u64, u64, PathBuf)>,
}

impl LogView {
    pub fn open(dir: &Path) -> Self {
        let head_path = dir.join(HEAD);
        let head_len = fs::metadata(&head_path).map_or(0, |meta| meta.len());
        let mut segments = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(start) = name
                    .to_str()
                    .and_then(|name| name.strip_prefix(SEGMENT_PREFIX))
                    .and_then(|start| start.parse::<u64>().ok())
                else {
                    continue;
                };
                let len = entry.metadata().map_or(0, |meta| meta.len());
                segments.push((start, len, entry.path()));
            }
        }
        segments.sort_by_key(|(start, _, _)| *start);
        Self {
            head_path,
            head_len,
            segments,
        }
    }

    /// Сколько байт вывода записано всего.
    pub fn total(&self) -> u64 {
        self.segments
            .last()
            .map_or(self.head_len, |(start, len, _)| start + len)
    }

    /// Сколько байт между головой и первым сохранённым куском хвоста пропало.
    pub fn omitted(&self) -> u64 {
        match self.segments.first() {
            Some((start, _, _)) => start.saturating_sub(self.head_len),
            None => 0,
        }
    }

    pub fn truncated(&self) -> bool {
        self.omitted() > 0
    }

    /// Байты потока с позиции `from` не больше `limit`; пропавший кусок пропускается:
    /// позиция следующего байта возвращается вместе с данными.
    pub fn read_from(&self, from: u64, limit: usize) -> (u64, Vec<u8>) {
        let mut at = from;
        if at < self.head_len {
            let want = usize::try_from(self.head_len - at)
                .unwrap_or(usize::MAX)
                .min(limit);
            return (at, read_range(&self.head_path, at, want));
        }
        for (start, len, path) in &self.segments {
            if at < *start {
                at = *start;
            }
            if at < start + len {
                let want = usize::try_from(start + len - at)
                    .unwrap_or(usize::MAX)
                    .min(limit);
                return (at, read_range(path, at - start, want));
            }
        }
        (at, Vec::new())
    }

    /// Последние `limit` байт потока из того, что сохранено.
    pub fn tail(&self, limit: usize) -> Vec<u8> {
        let total = self.total();
        let mut from = total.saturating_sub(limit as u64);
        let mut out = Vec::new();
        while out.len() < limit && from < total {
            // Начало окна могло пропасть: чтение само перескакивает к первому сохранённому байту.
            let (at, bytes) = self.read_from(from, limit - out.len());
            if bytes.is_empty() {
                break;
            }
            from = at + bytes.len() as u64;
            out.extend(bytes);
        }
        out
    }

    /// Голова, отметка о пропуске и хвост: то, что видит человек.
    pub fn assemble(&self, mut out: impl Write) -> io::Result<()> {
        copy_file(&self.head_path, &mut out)?;
        let omitted = self.omitted();
        if omitted > 0 {
            write!(out, "\n[... {omitted} bytes omitted ...]\n")?;
        }
        for (_, _, path) in &self.segments {
            copy_file(path, &mut out)?;
        }
        Ok(())
    }
}

fn read_range(path: &Path, offset: u64, want: usize) -> Vec<u8> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return Vec::new();
    }
    let mut buffer = vec![0; want];
    let mut filled = 0;
    while filled < want {
        match file.read(&mut buffer[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(read) => filled += read,
        }
    }
    buffer.truncate(filled);
    buffer
}

fn copy_file(path: &Path, out: &mut impl Write) -> io::Result<()> {
    match File::open(path) {
        Ok(mut file) => io::copy(&mut file, out).map(|_| ()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory_size(dir: &Path) -> u64 {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum()
    }

    fn stream(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| b'a' + u8::try_from(i % 26).unwrap())
            .collect()
    }

    #[test]
    fn small_output_is_kept_whole_with_no_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = LogWriter::create(dir.path(), 1024).unwrap();
        writer.write(b"hello ").unwrap();
        writer.write(b"world\n").unwrap();
        let view = LogView::open(dir.path());
        assert_eq!(view.total(), 12);
        assert!(!view.truncated());
        let mut out = Vec::new();
        view.assemble(&mut out).unwrap();
        assert_eq!(out, b"hello world\n");
        assert_eq!(view.tail(5), b"orld\n");
        assert_eq!(view.read_from(6, 100), (6, b"world\n".to_vec()));
    }

    #[test]
    fn disk_use_stays_under_the_cap_and_head_marker_tail_survive() {
        let dir = tempfile::tempdir().unwrap();
        let cap = 4096;
        let mut writer = LogWriter::create(dir.path(), cap).unwrap();
        let data = stream(200_000);
        for piece in data.chunks(777) {
            writer.write(piece).unwrap();
            assert!(directory_size(dir.path()) <= cap, "cap is hard");
        }
        let view = LogView::open(dir.path());
        assert_eq!(view.total(), data.len() as u64);
        assert!(view.truncated());

        let mut assembled = Vec::new();
        view.assemble(&mut assembled).unwrap();
        let text = String::from_utf8(assembled).unwrap();
        let (head_cap, seg_cap) = caps(cap);
        assert!(text.starts_with(std::str::from_utf8(&data[..head_cap as usize]).unwrap()));
        assert!(text.contains(&format!("[... {} bytes omitted ...]", view.omitted())));
        assert!(text.ends_with(std::str::from_utf8(&data[data.len() - 100..]).unwrap()));
        // Хвост не короче сегмента: пользователь видит конец вывода.
        let tail = view.tail(seg_cap as usize);
        assert_eq!(tail, &data[data.len() - seg_cap as usize..]);
        assert_eq!(
            head_cap + view.omitted() + (view.total() - head_cap - view.omitted()),
            view.total()
        );
    }

    #[test]
    fn reading_from_a_dropped_position_skips_to_the_first_kept_byte() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = LogWriter::create(dir.path(), 400).unwrap();
        let data = stream(5_000);
        writer.write(&data).unwrap();
        let view = LogView::open(dir.path());
        let (at, bytes) = view.read_from(150, 50);
        assert!(at > 150, "position moved past the dropped bytes: {at}");
        assert_eq!(bytes, &data[at as usize..at as usize + bytes.len()]);
        // Читая подряд с нуля, потребитель видит голову, затем прыжок, затем хвост.
        let mut position = 0;
        let mut seen = Vec::new();
        loop {
            let (at, bytes) = view.read_from(position, 64);
            if bytes.is_empty() {
                break;
            }
            seen.extend_from_slice(&bytes);
            position = at + bytes.len() as u64;
        }
        assert_eq!(position, data.len() as u64);
        assert_eq!(
            seen.len() as u64,
            view.total() - view.omitted(),
            "everything except the omitted gap"
        );
    }

    #[test]
    fn second_writer_view_sees_growth_without_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = LogWriter::create(dir.path(), 1 << 20).unwrap();
        writer.write(b"one").unwrap();
        assert_eq!(LogView::open(dir.path()).total(), 3);
        writer.write(b"two").unwrap();
        assert_eq!(LogView::open(dir.path()).total(), 6);
        assert_eq!(writer.total(), 6);
    }
}
