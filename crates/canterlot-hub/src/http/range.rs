//! Одиночные байтовые диапазоны при выдаче файлов (RFC 9110, раздел 14).
//!
//! Плеер браузера перематывает ролик запросами `Range`, а iOS Safari не играет ролик,
//! если сервер не отвечает на диапазоны кодом 206. Выдаётся только один диапазон: заголовок с
//! несколькими диапазонами, чужой единицей или неверной записью игнорируется, ответ идёт целиком.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::error::{ApiError, ErrorCode};
use crate::attachments::blobstore::IO_CHUNK_BYTES;

/// Что ответить на запрос с заголовком `Range`.
#[derive(Debug, PartialEq, Eq)]
pub enum Selection {
    /// Весь файл: диапазона нет, он не наш или условие `If-Range` не выполнено.
    Full,
    /// Байты `start..=end`, оба конца внутри файла.
    Partial { start: u64, end: u64 },
    /// Диапазон целиком за пределами файла: 416.
    Unsatisfiable,
}

/// Число без знака и пробелов, как в `byte-range-spec`.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() || text.len() > 19 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Условие `If-Range` выполнено, если оно несёт тот же сильный ETag. Слабый ETag и дата
/// не подходят: диапазон по ним не отдаётся, клиент получает файл целиком.
fn if_range_holds(request: &HeaderMap, etag: &str) -> bool {
    let mut values = request.get_all(header::IF_RANGE).iter();
    match (values.next(), values.next()) {
        (None, _) => true,
        (Some(value), None) => value.to_str().is_ok_and(|value| value.trim() == etag),
        _ => false,
    }
}

/// Выбор по заголовкам запроса: `size` — длина файла, `etag` — его сильный ETag в кавычках.
pub fn select(request: &HeaderMap, size: u64, etag: &str) -> Selection {
    let mut values = request.get_all(header::RANGE).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Selection::Full;
    };
    let Ok(value) = value.to_str() else {
        return Selection::Full;
    };
    let value = value.trim();
    let Some((unit, spec)) = value.split_once('=') else {
        return Selection::Full;
    };
    if !unit.trim().eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return Selection::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Selection::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    // Запись проверяется целиком до условия: чужое и неверное игнорируется, а не отвергается.
    let parsed = match (first.is_empty(), last.is_empty()) {
        (true, true) => return Selection::Full,
        (true, false) => match number(last) {
            Some(length) => (None, length),
            None => return Selection::Full,
        },
        (false, true) => match number(first) {
            Some(start) => (Some(start), u64::MAX),
            None => return Selection::Full,
        },
        (false, false) => match (number(first), number(last)) {
            (Some(start), Some(end)) if start <= end => (Some(start), end),
            _ => return Selection::Full,
        },
    };
    if !if_range_holds(request, etag) {
        return Selection::Full;
    }
    match parsed {
        // `-n`: последние n байт; `-0` и пустой файл удовлетворить нечем.
        (None, length) => {
            if length == 0 || size == 0 {
                Selection::Unsatisfiable
            } else {
                Selection::Partial {
                    start: size.saturating_sub(length),
                    end: size - 1,
                }
            }
        }
        (Some(start), end) => {
            if start >= size {
                Selection::Unsatisfiable
            } else {
                Selection::Partial {
                    start,
                    end: end.min(size - 1),
                }
            }
        }
    }
}

/// Тело файла или его части потоком: целиком в память оно не читается.
pub async fn respond(
    mut file: tokio::fs::File,
    size: u64,
    request: &HeaderMap,
    etag: &str,
    mut headers: HeaderMap,
) -> Result<Response, ApiError> {
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    match select(request, size, etag) {
        Selection::Full => {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
            Ok((StatusCode::OK, headers, stream(file, size)).into_response())
        }
        Selection::Partial { start, end } => {
            let length = end - start + 1;
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|_| ApiError::new(ErrorCode::Internal))?;
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
            headers.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes {start}-{end}/{size}"))
                    .map_err(|_| ApiError::new(ErrorCode::Internal))?,
            );
            Ok((StatusCode::PARTIAL_CONTENT, headers, stream(file, length)).into_response())
        }
        Selection::Unsatisfiable => {
            // Тело и тип не нужны; защитные заголовки остаются, кеш не обещается.
            let mut refusal = HeaderMap::new();
            for name in [
                header::ACCEPT_RANGES,
                header::X_CONTENT_TYPE_OPTIONS,
                header::CONTENT_SECURITY_POLICY,
            ] {
                if let Some(value) = headers.get(&name) {
                    refusal.insert(name, value.clone());
                }
            }
            refusal.insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}"))
                    .map_err(|_| ApiError::new(ErrorCode::Internal))?,
            );
            Ok((StatusCode::RANGE_NOT_SATISFIABLE, refusal).into_response())
        }
    }
}

fn stream(file: tokio::fs::File, length: u64) -> Body {
    Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
        file.take(length),
        IO_CHUNK_BYTES,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ETAG: &str = "\"abc\"";

    fn request(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    fn pick(range: &str, size: u64) -> Selection {
        select(&request(&[("range", range)]), size, ETAG)
    }

    #[test]
    fn closed_open_and_suffix_ranges_are_clamped_to_the_file() {
        let partial = |start, end| Selection::Partial { start, end };
        assert_eq!(pick("bytes=0-0", 10), partial(0, 0));
        assert_eq!(pick("bytes=2-5", 10), partial(2, 5));
        assert_eq!(pick("bytes=2-", 10), partial(2, 9));
        assert_eq!(pick("bytes=2-99", 10), partial(2, 9));
        assert_eq!(pick("bytes=-3", 10), partial(7, 9));
        assert_eq!(pick("bytes=-99", 10), partial(0, 9));
        assert_eq!(pick("BYTES=1-2", 10), partial(1, 2));
        assert_eq!(pick(" bytes = 1 - 2 ", 10), partial(1, 2));
    }

    #[test]
    fn ranges_outside_the_file_are_unsatisfiable() {
        assert_eq!(pick("bytes=10-", 10), Selection::Unsatisfiable);
        assert_eq!(pick("bytes=10-20", 10), Selection::Unsatisfiable);
        assert_eq!(pick("bytes=-0", 10), Selection::Unsatisfiable);
        assert_eq!(pick("bytes=0-", 0), Selection::Unsatisfiable);
        assert_eq!(pick("bytes=-5", 0), Selection::Unsatisfiable);
    }

    #[test]
    fn foreign_or_malformed_headers_are_ignored() {
        for value in [
            "bytes=0-1,3-4",
            "items=0-1",
            "bytes=",
            "bytes=-",
            "bytes=5-2",
            "bytes=a-b",
            "bytes=1-2-3",
            "bytes=+1-2",
            "bytes=99999999999999999999-",
            "0-1",
            "",
        ] {
            assert_eq!(pick(value, 10), Selection::Full, "{value}");
        }
        assert_eq!(select(&HeaderMap::new(), 10, ETAG), Selection::Full);
        let twice = request(&[("range", "bytes=0-1"), ("range", "bytes=2-3")]);
        assert_eq!(select(&twice, 10, ETAG), Selection::Full);
    }

    #[test]
    fn if_range_needs_the_same_strong_etag() {
        let with = |condition: &str| {
            select(
                &request(&[("range", "bytes=0-1"), ("if-range", condition)]),
                10,
                ETAG,
            )
        };
        assert_eq!(with(ETAG), Selection::Partial { start: 0, end: 1 });
        assert_eq!(with("\"other\""), Selection::Full);
        assert_eq!(with("W/\"abc\""), Selection::Full);
        assert_eq!(with("Wed, 21 Oct 2015 07:28:00 GMT"), Selection::Full);
        // Устаревшее условие даёт файл целиком даже для диапазона за его концом.
        let stale = request(&[("range", "bytes=50-"), ("if-range", "\"other\"")]);
        assert_eq!(select(&stale, 10, ETAG), Selection::Full);
    }
}
